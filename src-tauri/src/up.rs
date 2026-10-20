use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::docker;
use crate::git;
use crate::model::{resolve_env, Service, Settings, Workspace};
use crate::procs::{Conflict, Runtime};
use crate::util::{now_ms, run};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StageItem {
    pub name: String,
    /// pending | running | passed | warn | failed | skipped
    pub status: String,
    pub detail: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub id: String,
    pub title: String,
    /// pending | running | passed | failed | skipped
    pub status: String,
    pub detail: String,
    pub items: Vec<StageItem>,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpRun {
    pub workspace_id: String,
    pub workspace_name: String,
    pub profile: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// running | success | failed | cancelled
    pub status: String,
    pub stages: Vec<Stage>,
    pub conflict: Option<Conflict>,
    pub error: Option<String>,
}

fn stage(id: &str, title: &str) -> Stage {
    Stage { id: id.into(), title: title.into(), status: "pending".into(), detail: String::new(), items: vec![], started_ms: None, finished_ms: None }
}

fn item(name: &str, status: &str, detail: impl Into<String>) -> StageItem {
    StageItem { name: name.into(), status: status.into(), detail: detail.into() }
}

struct Ctx {
    rt: Arc<Runtime>,
}

impl Ctx {
    fn update<F: FnOnce(&mut UpRun)>(&self, f: F) {
        if let Some(r) = self.rt.up.lock().unwrap().as_mut() {
            f(r);
        }
    }
    fn stage<F: FnOnce(&mut Stage)>(&self, idx: usize, f: F) {
        self.update(|r| f(&mut r.stages[idx]));
    }
    fn cancelled(&self) -> bool {
        self.rt.up_cancel.load(Ordering::SeqCst)
    }
}

/// Dependency order (Kahn). Unknown dependencies are ignored; cycles are an error.
pub fn order(services: &[Service]) -> Result<Vec<Service>, String> {
    let names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    for s in services {
        indeg.insert(&s.name, s.depends_on.iter().filter(|d| names.contains(d.as_str()) && **d != s.name).count());
    }
    let mut out: Vec<Service> = Vec::new();
    let mut done: HashSet<String> = HashSet::new();
    while out.len() < services.len() {
        let ready: Vec<&Service> = services
            .iter()
            .filter(|s| !done.contains(&s.name) && s.depends_on.iter().all(|d| !names.contains(d.as_str()) || *d == s.name || done.contains(d)))
            .collect();
        if ready.is_empty() {
            let left: Vec<&str> = services.iter().filter(|s| !done.contains(&s.name)).map(|s| s.name.as_str()).collect();
            return Err(format!("Circular dependency between: {}", left.join(", ")));
        }
        for s in ready {
            done.insert(s.name.clone());
            out.push(s.clone());
        }
    }
    let _ = indeg;
    Ok(out)
}

pub fn start(rt: Arc<Runtime>, ws: Workspace, settings: Settings) -> Result<(), String> {
    {
        let g = rt.up.lock().unwrap();
        if g.as_ref().map_or(false, |r| r.status == "running") {
            return Err("An Up run is already in progress".into());
        }
    }
    if ws.services.is_empty() {
        return Err("This workspace has no services yet".into());
    }
    let ordered = order(&ws.services)?;

    rt.up_cancel.store(false, Ordering::SeqCst);
    *rt.up.lock().unwrap() = Some(UpRun {
        workspace_id: ws.id.clone(),
        workspace_name: ws.name.clone(),
        profile: ws.profile.clone(),
        started_ms: now_ms(),
        finished_ms: None,
        status: "running".into(),
        stages: vec![
            stage("prereq", "Check tools & prerequisites"),
            stage("sync", "Sync repositories"),
            stage("env", "Load environment & secrets"),
            stage("infra", "Start dependencies (Docker)"),
            stage("services", "Start services"),
        ],
        conflict: None,
        error: None,
    });

    std::thread::spawn(move || {
        let ctx = Ctx { rt };
        let result = pipeline(&ctx, &ws, &settings, &ordered);
        let cancelled = ctx.cancelled();
        ctx.update(|r| {
            r.finished_ms = Some(now_ms());
            match &result {
                _ if cancelled => r.status = "cancelled".into(),
                Ok(()) => r.status = "success".into(),
                Err(e) => {
                    r.status = "failed".into();
                    r.error = Some(e.clone());
                }
            }
            for s in r.stages.iter_mut() {
                if s.status == "pending" || s.status == "running" {
                    s.status = if s.status == "running" && !cancelled { "failed".into() } else { "skipped".into() };
                }
            }
        });
    });
    Ok(())
}

fn begin(ctx: &Ctx, idx: usize) {
    ctx.stage(idx, |s| {
        s.status = "running".into();
        s.started_ms = Some(now_ms());
    });
}
fn finish(ctx: &Ctx, idx: usize, status: &str, detail: impl Into<String>) {
    let d = detail.into();
    ctx.stage(idx, |s| {
        s.status = status.into();
        s.detail = d;
        s.finished_ms = Some(now_ms());
    });
}
fn set_items(ctx: &Ctx, idx: usize, items: Vec<StageItem>) {
    ctx.stage(idx, |s| s.items = items);
}
fn set_item(ctx: &Ctx, idx: usize, i: usize, status: &str, detail: impl Into<String>) {
    let d = detail.into();
    ctx.stage(idx, |s| {
        if let Some(it) = s.items.get_mut(i) {
            it.status = status.into();
            it.detail = d;
        }
    });
}

fn pipeline(ctx: &Ctx, ws: &Workspace, settings: &Settings, ordered: &[Service]) -> Result<(), String> {
    let infra: Vec<&Service> = ordered.iter().filter(|s| s.kind == "infra").collect();
    let apps: Vec<&Service> = ordered.iter().filter(|s| s.kind != "infra").collect();

    // 1. prerequisites
    begin(ctx, 0);
    let mut items: Vec<StageItem> = Vec::new();
    let mut failed: Option<String> = None;

    let repos: Vec<String> = {
        let mut seen = HashSet::new();
        apps.iter().map(|s| s.repo_path.clone()).filter(|p| !p.is_empty() && seen.insert(p.clone())).collect()
    };
    if !repos.is_empty() {
        match run("git", &["--version"], None) {
            Ok(o) if o.ok => items.push(item("git", "passed", o.stdout.trim().to_string())),
            _ => items.push(item("git", "warn", "git not found: repository sync will be skipped")),
        }
    }
    if !infra.is_empty() {
        if docker::is_running() {
            items.push(item("docker", "passed", "daemon reachable"));
        } else {
            items.push(item("docker", "failed", "Docker daemon is not running: start Docker Desktop"));
            failed = Some("Docker is required for infra services but is not running".into());
        }
    }
    for s in &apps {
        if s.command.trim().is_empty() {
            items.push(item(&s.name, "failed", "no start command configured"));
            failed.get_or_insert(format!("`{}` has no start command", s.name));
        } else if !Path::new(&s.repo_path).is_dir() {
            items.push(item(&s.name, "failed", format!("folder not found: {}", s.repo_path)));
            failed.get_or_insert(format!("Folder for `{}` does not exist", s.name));
        }
    }
    // Port pre-flight: ports held by something we did NOT start are conflicts.
    let mut conflict: Option<Conflict> = None;
    for s in ordered {
        if let Some(p) = s.port {
            if ctx.rt.is_alive(&ws.id, &s.name) {
                continue;
            }
            if let Some(o) = crate::ports::owner(p) {
                let who = match &o.container {
                    Some(c) => format!("Docker container `{c}`"),
                    None => format!("{} (PID {})", o.process, o.pid),
                };
                items.push(item(&format!("{}:{}", s.name, p), "failed", format!("port in use by {who}")));
                failed.get_or_insert(format!("Port {p} needed by `{}` is held by {who}", s.name));
                if conflict.is_none() {
                    conflict = Some(Conflict { service: s.name.clone(), port: p, pid: o.pid, process: o.process, container: o.container });
                }
            }
        }
    }
    if let Some(c) = conflict {
        ctx.update(|r| r.conflict = Some(c));
    }
    set_items(ctx, 0, items);
    if let Some(e) = failed {
        finish(ctx, 0, "failed", e.clone());
        return Err(e);
    }
    finish(ctx, 0, "passed", "all checks passed");
    if ctx.cancelled() {
        return Ok(());
    }

    // 2. repo sync (never fails the run)
    begin(ctx, 1);
    if !ws.sync_before_up || repos.is_empty() {
        finish(ctx, 1, "skipped", if repos.is_empty() { "no repositories" } else { "disabled for this workspace" });
    } else {
        let mut items: Vec<StageItem> = repos.iter().map(|r| item(&short(r), "pending", "")).collect();
        set_items(ctx, 1, items.clone());
        let (mut synced, mut skipped) = (0, 0);
        for (i, r) in repos.iter().enumerate() {
            if ctx.cancelled() {
                return Ok(());
            }
            set_item(ctx, 1, i, "running", "fetching…");
            let res = git::pull_safe(r);
            let st = match res.outcome.as_str() {
                "synced" | "upToDate" => {
                    synced += 1;
                    "passed"
                }
                "notRepo" => "skipped",
                _ => {
                    skipped += 1;
                    "warn"
                }
            };
            items[i] = item(&short(r), st, res.message.clone());
            set_item(ctx, 1, i, st, res.message);
        }
        finish(ctx, 1, "passed", format!("{synced} synced, {skipped} need attention"));
    }

    // 3. env
    begin(ctx, 2);
    let mut env_items = Vec::new();
    let mut empty = 0;
    for s in ordered {
        let vars = resolve_env(ws, s, &ws.profile);
        let blank = vars.iter().filter(|(_, v)| v.trim().is_empty()).count();
        empty += blank;
        env_items.push(item(&s.name, if blank > 0 { "warn" } else { "passed" }, format!("{} variable(s){}", vars.len(), if blank > 0 { format!(", {blank} empty") } else { String::new() })));
    }
    set_items(ctx, 2, env_items);
    finish(ctx, 2, "passed", format!("profile `{}`{}", ws.profile, if empty > 0 { format!(", {empty} empty value(s)") } else { String::new() }));

    // 4. infra, 5. services
    run_group(ctx, 3, ws, settings, &infra, "no Docker dependencies")?;
    run_group(ctx, 4, ws, settings, &apps, "no services")?;
    Ok(())
}

fn short(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
}

fn run_group(ctx: &Ctx, idx: usize, ws: &Workspace, settings: &Settings, list: &[&Service], empty_msg: &str) -> Result<(), String> {
    begin(ctx, idx);
    if list.is_empty() {
        finish(ctx, idx, "skipped", empty_msg);
        return Ok(());
    }
    set_items(ctx, idx, list.iter().map(|s| item(&s.name, "pending", "")).collect());
    for (i, s) in list.iter().enumerate() {
        if ctx.cancelled() {
            return Ok(());
        }
        let t0 = Instant::now();
        set_item(ctx, idx, i, "running", "starting…");
        if let Err(e) = ctx.rt.start_service(ws, s) {
            set_item(ctx, idx, i, "failed", e.message.clone());
            if let Some(c) = e.conflict {
                ctx.update(|r| r.conflict = Some(c));
            }
            finish(ctx, idx, "failed", format!("`{}` failed to start", s.name));
            return Err(format!("`{}`: {}", s.name, e.message));
        }
        // wait for readiness
        let deadline = Instant::now() + Duration::from_secs(settings.health_timeout_secs.max(5));
        loop {
            if ctx.cancelled() {
                return Ok(());
            }
            if ctx.rt.is_ready(&ws.id, &s.name) {
                let secs = t0.elapsed().as_secs_f32();
                let live = ctx.rt.live_ports(&ws.id, &s.name);
                let at = |l: &[u16]| l.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(", ");
                let detail = match s.port {
                    Some(p) if live.is_empty() || live.contains(&p) => format!(":{p} ready in {secs:.1}s"),
                    Some(p) => format!("listening on {} instead of :{p} ({secs:.1}s)", at(&live)),
                    None if !live.is_empty() => format!("listening on {} ({secs:.1}s)", at(&live)),
                    None => format!("running ({secs:.1}s)"),
                };
                set_item(ctx, idx, i, "passed", detail);
                break;
            }
            if let Some((false, code)) = ctx.rt.exit_state(&ws.id, &s.name) {
                let why = ctx.rt.recent_error(&ws.id, &s.name).unwrap_or_default();
                let detail = format!("exited with code {}{}", code.map_or("?".into(), |c| c.to_string()), if why.is_empty() { String::new() } else { format!(": {why}") });
                set_item(ctx, idx, i, "failed", detail.clone());
                finish(ctx, idx, "failed", format!("`{}` exited during startup", s.name));
                return Err(format!("`{}` {}", s.name, detail));
            }
            if Instant::now() > deadline {
                let msg = match s.port {
                    Some(p) => format!("port :{p} did not open within {}s", settings.health_timeout_secs),
                    None => "did not become ready in time".to_string(),
                };
                set_item(ctx, idx, i, "failed", msg.clone());
                finish(ctx, idx, "failed", format!("`{}` timed out", s.name));
                return Err(format!("`{}` {msg}", s.name));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    finish(ctx, idx, "passed", format!("{} started", list.len()));
    Ok(())
}
