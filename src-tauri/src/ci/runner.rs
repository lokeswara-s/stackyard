//! Executes one job: checkout, sandbox (container or host), steps, artifacts, cache.

use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::engine::*;
use super::exec::{self, Sandbox, Shell};
use super::gitlab::SEP;
use super::groovy::{self, Scope, GV};
use super::ir::*;
use super::jenkins::{CRED, EXPR, GSTR};
use crate::util::{now_ms, run as run_cmd};

pub enum Outcome {
    Passed,
    Warning(String),
    Failed(String),
    Canceled,
}

pub enum StepRes {
    Ok,
    Failed(String),
    Skipped(String),
    Cancelled,
}

#[derive(Clone)]
pub struct GhScope {
    pub inputs: Value,
    pub steps: Map<String, Value>,
    pub action_path: String,
}

pub enum PostAction {
    CacheSave { key: String, paths: Vec<String>, hit: bool },
    NodeAction { dir_in: String, script: String, env: Vec<Kv>, name: String },
}

pub struct St {
    pub failed: bool,
    pub fail_msg: Option<String>,
    pub cancelled: bool,
    pub unstable: bool,
    pub catch_failed: bool,
    pub gh_env: Vec<Kv>,
    pub path_add: Vec<String>,
    pub scopes: Vec<GhScope>,
    pub post: Vec<PostAction>,
    pub jvars: HashMap<String, String>,
    pub jenv: Vec<Kv>,
    pub step_files: u32,
}

pub struct JobCtx<'a> {
    pub run: &'a Arc<Run>,
    pub idx: usize,
    pub job: &'a Job,
    pub sb: Sandbox,
    pub env: Vec<Kv>,
    pub masks: Mutex<Vec<String>>,
    pub deadline: Option<Instant>,
    pub cur: Cell<i32>,
    pub image: Option<String>,
    pub sources: RefCell<Vec<usize>>,
}

struct Teardown {
    docker: bool,
    containers: Vec<String>,
    network: Option<String>,
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if self.docker {
            for c in &self.containers {
                exec::remove_container(c);
            }
            if let Some(n) = &self.network {
                exec::remove_network(n);
            }
        }
    }
}

pub fn container_name(run: &Run, idx: usize) -> String {
    format!("dwci-{}-{}", run.id.trim_start_matches("run-"), idx)
}

/// Stop everything a job started (containers, services, network).
pub fn kill_job(run: &Run, idx: usize) {
    if !run.docker {
        return;
    }
    let base = container_name(run, idx);
    exec::remove_container(&base);
    if let Ok(o) = exec::docker(&["ps", "-aq", "--filter", &format!("name=^{base}-")]) {
        for id in o.stdout.split_whitespace() {
            exec::remove_container(id);
        }
    }
    exec::remove_network(&format!("{base}-net"));
}

// ============================== helpers ==============================

pub fn collect_files(base: &Path, patterns: &[String], excludes: &[String]) -> Vec<PathBuf> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().map(|n| n == ".git").unwrap_or(false) {
                        continue;
                    }
                    walk(&p, base, out);
                } else if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    let mut out: Vec<PathBuf> = vec![];
    for pat in patterns {
        let pat = pat.trim().trim_start_matches("./").trim_start_matches('/');
        if pat.is_empty() {
            continue;
        }
        let full = base.join(pat);
        let mut matches: Vec<PathBuf> = vec![];
        if full.exists() {
            matches.push(full);
        } else if let Ok(it) = glob::glob(&format!("{}/{}", base.to_string_lossy().replace('\\', "/"), pat)) {
            matches.extend(it.flatten());
        }
        for m in matches {
            if m.is_dir() {
                walk(&m, base, &mut out);
            } else if let Ok(rel) = m.strip_prefix(base) {
                out.push(rel.to_path_buf());
            }
        }
    }
    let ex: Vec<glob::Pattern> = excludes.iter().filter_map(|e| glob::Pattern::new(e.trim_start_matches("./")).ok()).collect();
    out.retain(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        !ex.iter().any(|g| g.matches(&s))
    });
    out.sort();
    out.dedup();
    out
}

pub fn copy_files(from: &Path, to: &Path, rels: &[PathBuf]) -> usize {
    let mut n = 0;
    for r in rels {
        let src = from.join(r);
        let dst = to.join(r);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::copy(&src, &dst).is_ok() {
            n += 1;
        }
    }
    n
}

fn copy_dir_all(from: &Path, to: &Path) -> usize {
    let rels = collect_files(from, &["**/*".to_string(), "*".to_string()], &[]);
    let all = if rels.is_empty() { collect_files(from, &[".".to_string()], &[]) } else { rels };
    let _ = std::fs::create_dir_all(to);
    copy_files(from, to, &all)
}

fn hash_str(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn hash_files(base: &Path, patterns: &[String]) -> String {
    let files = collect_files(base, patterns, &[]);
    if files.is_empty() {
        return String::new();
    }
    let mut acc = String::new();
    for f in files {
        if let Ok(bytes) = std::fs::read(base.join(&f)) {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            acc.push_str(&format!("{:016x}", h.finish()));
        }
    }
    hash_str(&acc)
}

fn parse_dotenv(text: &str) -> Vec<Kv> {
    let mut out = vec![];
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            out.push(Kv::new(k.trim(), v.trim().trim_matches(|c| c == '"' || c == '\'')));
        }
    }
    out
}

fn fmt_dur(ms: u128) -> String {
    let s = ms / 1000;
    if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}.{}s", s, (ms % 1000) / 100)
    }
}

// ============================== workspace ==============================

fn git_run(args: &[&str], cwd: Option<&str>) -> Result<(), String> {
    let o = run_cmd("git", args, cwd)?;
    if o.ok {
        Ok(())
    } else {
        Err(o.error_text())
    }
}

fn prepare_source(run: &Run, dest: &Path) -> Result<(), String> {
    let repo = run.opts.repo_path.as_str();
    let t = &run.trig;
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    std::fs::create_dir_all(dest.parent().unwrap_or(dest)).map_err(|e| e.to_string())?;
    let d = dest.to_string_lossy().into_owned();
    git_run(&["-c", "core.autocrlf=false", "-c", "core.longpaths=true", "clone", "--no-hardlinks", "--no-checkout", "--quiet", repo, &d], None).map_err(|e| format!("git clone failed: {e}"))?;
    if !t.tag.is_empty() {
        git_run(&["-c", "core.autocrlf=false", "checkout", "--quiet", "--detach", &t.sha], Some(&d))?;
    } else if !t.branch.is_empty() {
        git_run(&["-c", "core.autocrlf=false", "checkout", "--quiet", "-B", &t.branch, &t.sha], Some(&d))?;
    } else {
        git_run(&["-c", "core.autocrlf=false", "checkout", "--quiet", "--detach", &t.sha], Some(&d))?;
    }
    if !t.origin_url.is_empty() {
        let _ = git_run(&["remote", "set-url", "origin", &t.origin_url], Some(&d));
    }
    let _ = git_run(&["config", "user.name", "Stackyard CI"], Some(&d));
    let _ = git_run(&["config", "user.email", "ci@stackyard.local"], Some(&d));
    let _ = git_run(&["config", "core.autocrlf", "false"], Some(&d));
    if t.working {
        // Overlay uncommitted changes: modified, added and untracked files, and deletions.
        let o = run_cmd("git", &["-C", repo, "status", "--porcelain=v1", "-z", "--untracked-files=all"], None)?;
        let parts: Vec<&str> = o.stdout.split('\0').collect();
        let mut i = 0;
        while i < parts.len() {
            let e = parts[i];
            i += 1;
            if e.len() < 4 {
                continue;
            }
            let (xy, path) = (&e[..2], &e[3..]);
            if xy.starts_with('R') || xy.starts_with('C') {
                i += 1; // the "from" path follows
            }
            let src = Path::new(repo).join(path);
            let dst = dest.join(path);
            if xy.contains('D') && !src.exists() {
                let _ = std::fs::remove_file(&dst);
            } else if src.is_file() {
                if let Some(p) = dst.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::copy(&src, &dst);
            }
        }
    }
    Ok(())
}

// ============================== env ==============================

pub struct JScope<'a> {
    pub env: &'a [Kv],
    pub params: &'a [Kv],
    pub vars: &'a HashMap<String, String>,
    pub run: &'a Run,
    pub result: String,
    pub ws: Option<&'a Path>,
}

impl<'a> Scope for JScope<'a> {
    fn var(&self, name: &str) -> Option<String> {
        if let Some(k) = name.strip_prefix("env.") {
            return get(self.env, k).map(String::from);
        }
        if let Some(k) = name.strip_prefix("params.") {
            return get(self.params, k).map(String::from);
        }
        match name {
            "currentBuild.number" => return Some(self.run.number.to_string()),
            "currentBuild.result" | "currentBuild.currentResult" => return Some(self.result.clone()),
            "currentBuild.displayName" => return Some(format!("#{}", self.run.number)),
            _ => {}
        }
        self.vars.get(name).cloned().or_else(|| get(self.env, name).map(String::from)).or_else(|| get(self.params, name).map(String::from))
    }
    fn typed(&self, name: &str) -> Option<GV> {
        let k = name.strip_prefix("params.")?;
        let p = self.run.pipeline.params.iter().find(|p| p.name == k)?;
        (p.kind == "boolean").then(|| GV::Bool(get(self.params, k).map(|v| v == "true").unwrap_or(false)))
    }
    fn call(&self, name: &str, args: &[GV]) -> Option<GV> {
        let a = |i: usize| args.get(i).map(|x| x.text()).unwrap_or_default();
        let t = &self.run.trig;
        Some(match name {
            "__branch" => GV::Bool(groovy::glob(&a(0), if t.branch.is_empty() { &t.ref_name } else { &t.branch })),
            "__tag" => GV::Bool(!t.tag.is_empty() && groovy::glob(&a(0), &t.tag)),
            "__buildingTag" => GV::Bool(!t.tag.is_empty()),
            "__changeRequest" => GV::Bool(t.event == "pull_request" || t.event == "merge_request"),
            "__changeset" => GV::Bool(t.changed.iter().any(|f| groovy::glob(&a(0), f))),
            "__triggeredBy" => GV::Bool(matches!(a(0).as_str(), "UserIdCause" | "user")),
            "isUnix" => GV::Bool(true),
            "fileExists" => GV::Bool(self.ws.map(|w| w.join(a(0)).exists()).unwrap_or(false)),
            _ => return None,
        })
    }
}

pub fn jtext(raw: &str, sc: &dyn Scope, run: &Run) -> String {
    if let Some(e) = raw.strip_prefix(EXPR) {
        return match groovy::eval(e, sc) {
            Ok(GV::Null) => String::new(),
            Ok(v) => v.text(),
            Err(_) => e.to_string(),
        };
    }
    if let Some(g) = raw.strip_prefix(GSTR) {
        return groovy::interpolate(g, sc);
    }
    if let Some(id) = raw.strip_prefix(CRED) {
        let (name, part) = match id.rsplit_once(':') {
            Some((n, p)) if p == "USR" || p == "PSW" => (n, p),
            _ => (id, ""),
        };
        let key = if part.is_empty() { name.to_string() } else { format!("{name}_{part}") };
        return run.ci_var(&key).or_else(|| run.ci_var(name)).unwrap_or_default();
    }
    raw.to_string()
}

fn jenkins_params(run: &Run) -> Vec<Kv> {
    let mut p: Vec<Kv> = run.pipeline.params.iter().map(|x| Kv::new(&x.name, &x.default)).collect();
    for kv in &run.opts.variables {
        put(&mut p, &kv.key, kv.value.clone());
    }
    p
}

fn jenkins_base_env(run: &Run, job: &Job, ws_in: &str) -> Vec<Kv> {
    let t = &run.trig;
    let n = run.number.to_string();
    let mut v: Vec<Kv> = vec![];
    let mut p = |k: &str, val: &str| put(&mut v, k, val);
    p("BUILD_NUMBER", &n);
    p("BUILD_ID", &n);
    p("BUILD_DISPLAY_NAME", &format!("#{n}"));
    p("JOB_NAME", &t.project_name);
    p("JOB_BASE_NAME", &t.project_name);
    p("BUILD_TAG", &format!("jenkins-{}-{n}", t.project_name));
    p("BUILD_URL", &format!("http://localhost:8080/job/{}/{n}/", t.project_name));
    p("JOB_URL", &format!("http://localhost:8080/job/{}/", t.project_name));
    p("JENKINS_URL", "http://localhost:8080/");
    p("RUN_DISPLAY_URL", &format!("http://localhost:8080/job/{}/{n}/display/redirect", t.project_name));
    p("WORKSPACE", ws_in);
    p("NODE_NAME", "built-in");
    p("NODE_LABELS", "built-in");
    p("EXECUTOR_NUMBER", "0");
    p("GIT_COMMIT", &t.sha);
    p("GIT_PREVIOUS_COMMIT", &t.before_sha);
    p("GIT_URL", &t.origin_url);
    if !t.branch.is_empty() {
        p("GIT_BRANCH", &format!("origin/{}", t.branch));
        p("BRANCH_NAME", &t.branch);
    }
    if !t.tag.is_empty() {
        p("TAG_NAME", &t.tag);
        p("BRANCH_NAME", &t.tag);
    }
    if t.event == "pull_request" || t.event == "merge_request" {
        p("CHANGE_ID", "1");
        p("CHANGE_BRANCH", &t.branch);
        p("CHANGE_TARGET", &t.default_branch);
        p("CHANGE_TITLE", &t.title);
    }
    let _ = job;
    v
}

/// Jenkins `environment {}` blocks resolve in order, each value seeing the earlier ones.
fn jenkins_resolve_env(run: &Run, base: &mut Vec<Kv>, params: &[Kv], list: &[Kv]) {
    for kv in list {
        let val = {
            let vars = HashMap::new();
            let sc = JScope { env: base, params, vars: &vars, run, result: "SUCCESS".into(), ws: None };
            jtext(&kv.value, &sc, run)
        };
        put(base, &kv.key, val);
    }
}

pub fn jenkins_when(run: &Run, idx: usize, cond: &str) -> Result<bool, String> {
    let job = &run.jobs[idx].job;
    let mut env = jenkins_base_env(run, job, "");
    let params = jenkins_params(run);
    for p in &params {
        put(&mut env, &p.key, p.value.clone());
    }
    jenkins_resolve_env(run, &mut env, &params, &run.pipeline.env);
    jenkins_resolve_env(run, &mut env, &params, &job.env);
    let vars = HashMap::new();
    let sc = JScope { env: &env, params: &params, vars: &vars, run, result: run.build_result.lock().unwrap().clone(), ws: None };
    groovy::eval(cond, &sc).map(|v| v.truthy())
}

fn gitlab_job_env(run: &Run, idx: usize, ws_in: &str) -> Vec<Kv> {
    let job = &run.jobs[idx].job;
    let mut env = gitlab_predefined(run.number, &run.trig, &run.opts);
    let jid = (run.number * 1000 + idx as u64 + 1).to_string();
    put(&mut env, "CI_JOB_ID", jid.clone());
    put(&mut env, "CI_JOB_NAME", job.name.clone());
    put(&mut env, "CI_JOB_NAME_SLUG", slug(&job.name));
    put(&mut env, "CI_JOB_STAGE", job.stage.clone());
    put(&mut env, "CI_JOB_STATUS", "running");
    put(&mut env, "CI_JOB_URL", format!("https://gitlab.local/{}/{}/-/jobs/{jid}", run.trig.namespace, run.trig.project_name));
    put(&mut env, "CI_JOB_TOKEN", "simulated-job-token");
    put(&mut env, "CI_PROJECT_DIR", ws_in);
    put(&mut env, "CI_BUILDS_DIR", ws_in.rsplit_once('/').map(|x| x.0).unwrap_or(ws_in));
    if let Some(img) = &job.image {
        put(&mut env, "CI_JOB_IMAGE", img.clone());
    }
    if let Some((i, n)) = job.node {
        put(&mut env, "CI_NODE_INDEX", i.to_string());
        put(&mut env, "CI_NODE_TOTAL", n.to_string());
    } else {
        put(&mut env, "CI_NODE_TOTAL", "1");
    }
    let mut vars = env.clone();
    for kv in &run.pipeline.env {
        put(&mut vars, &kv.key, kv.value.clone());
    }
    for kv in &job.env {
        put(&mut vars, &kv.key, kv.value.clone());
    }
    // dotenv reports from the jobs this one depends on
    for d in run.deps_of(idx) {
        for kv in run.jobs[d].dynamic.lock().unwrap().dotenv.iter() {
            put(&mut vars, &kv.key, kv.value.clone());
        }
    }
    for v in &run.ci_vars {
        put(&mut vars, &v.key, v.value.clone());
    }
    for kv in &run.opts.variables {
        put(&mut vars, &kv.key, kv.value.clone());
    }
    // References between variables are expanded (twice, to cover chains).
    for _ in 0..2 {
        let snapshot = vars.clone();
        for kv in vars.iter_mut() {
            if kv.value.contains('$') {
                kv.value = expand_vars(&kv.value, &snapshot);
            }
        }
    }
    vars
}

// ============================== run a job ==============================

pub fn run_job(run: &Arc<Run>, idx: usize) {
    let js = &run.jobs[idx];
    let max_attempts = js.job.retry + 1;
    let mut outcome = Outcome::Failed("not run".into());
    for attempt in 1..=max_attempts {
        {
            let mut d = js.dynamic.lock().unwrap();
            d.attempt = attempt;
            d.steps = js.job.steps.iter().map(|s| StepDyn { name: s.name.clone(), kind: kind_label(&s.kind).into(), status: "pending".into(), started_ms: None, finished_ms: None }).collect();
        }
        if attempt > 1 {
            run.log(idx, -1, &format!("--- Retrying ({attempt}/{max_attempts}) ---"));
        }
        outcome = run_attempt(run, idx);
        match &outcome {
            Outcome::Failed(_) if attempt < max_attempts && !run.cancel.load(Ordering::SeqCst) => continue,
            _ => break,
        }
    }
    let mut d = js.dynamic.lock().unwrap();
    d.finished_ms = Some(now_ms());
    match outcome {
        Outcome::Passed => d.status = "passed".into(),
        Outcome::Warning(n) => {
            d.status = "warning".into();
            d.note = Some(n);
        }
        Outcome::Failed(msg) => {
            if js.job.allow_failure {
                d.status = "warning".into();
                d.note = Some(format!("failed, but allowed to fail: {msg}"));
            } else {
                d.status = "failed".into();
                d.note = Some(msg);
                if run.pipeline.provider == "jenkins" {
                    *run.build_result.lock().unwrap() = "FAILURE".into();
                }
            }
        }
        Outcome::Canceled => {
            d.status = "canceled".into();
            d.note = Some("cancelled".into());
        }
    }
    drop(d);
    // Free the job's checkout (artifacts were already copied out).
    if !js.job.shared_workspace {
        let dir = run.dir.join("jobs").join(job_dir_name(&js.job.id));
        std::thread::spawn(move || {
            let _ = std::fs::remove_dir_all(dir);
        });
    }
}

/// Image for jobs that do not name one (the same one GitHub `ubuntu-latest` uses).
pub const DEFAULT_IMAGE: &str = "catthehacker/ubuntu:act-latest";

struct Runtime {
    image: Option<String>,
    note: Option<String>,
}

fn resolve_runtime(run: &Run, idx: usize) -> Runtime {
    let job = &run.jobs[idx].job;
    if !run.docker {
        return Runtime { image: None, note: Some(if run.opts.runner == "host" { "runner: this machine".into() } else { "Docker is not running; running on this machine".into() }) };
    }
    match run.pipeline.provider.as_str() {
        "github" => {
            if let Some(i) = &job.image {
                let i = super::gh::interp_job(run, idx, i);
                return Runtime { image: Some(i), note: None };
            }
            let ro = job.runs_on.clone().map(|r| super::gh::interp_job(run, idx, &r)).unwrap_or_else(|| "ubuntu-latest".into());
            let l = ro.to_lowercase();
            if l.starts_with("ubuntu") {
                let custom = run.opts.ubuntu_image.trim();
                let image = if !custom.is_empty() {
                    custom.to_string()
                } else if l.contains("22.04") {
                    "catthehacker/ubuntu:act-22.04".into()
                } else if l.contains("20.04") {
                    "catthehacker/ubuntu:act-20.04".into()
                } else if l.contains("24.04") {
                    "catthehacker/ubuntu:act-24.04".into()
                } else {
                    "catthehacker/ubuntu:act-latest".into()
                };
                Runtime { image: Some(image), note: None }
            } else {
                // Windows / macOS runners cannot be reproduced; a Linux container is closer than Git Bash on this PC.
                let custom = run.opts.ubuntu_image.trim();
                let image = if custom.is_empty() { DEFAULT_IMAGE } else { custom };
                Runtime { image: Some(image.to_string()), note: Some(format!("`runs-on: {ro}` cannot be emulated; running in a Linux container (`{image}`) instead")) }
            }
        }
        _ => match &job.image {
            Some(i) => Runtime { image: Some(i.clone()), note: None },
            None => {
                // The server's runner picks its own default image; use the one chosen in the run options, else a
                // fat Ubuntu image that has the usual tools (git, make, curl, node, python, docker CLI).
                let d = run.opts.ubuntu_image.trim();
                let image = if d.is_empty() { DEFAULT_IMAGE } else { d };
                Runtime { image: Some(image.to_string()), note: Some(format!("no image configured for this job; using the default image `{image}`")) }
            }
        },
    }
}

fn run_attempt(run: &Arc<Run>, idx: usize) -> Outcome {
    let js = &run.jobs[idx];
    let job = &js.job;
    let provider = run.pipeline.provider.as_str();
    let log = |t: &str| run.log(idx, -1, t);
    let t0 = Instant::now();

    let rt = resolve_runtime(run, idx);
    {
        js.dynamic.lock().unwrap().runner = match &rt.image {
            Some(i) => format!("container: {i}"),
            None => "this machine".into(),
        };
    }
    log(&format!("Preparing job `{}` ({})", job.name, provider));
    if let Some(n) = &rt.note {
        log(n);
    }

    // ---- workspace ----
    let project = if provider == "gitlab" { run.trig.project_name.clone() } else { run.trig.project_name.clone() };
    let ws_host: PathBuf = if job.shared_workspace {
        let mut g = run.shared_ws.lock().unwrap();
        let dir = run.dir.join("ws").join(&project);
        if g.is_none() {
            log("Checking out the repository (clean clone of the commit)...");
            if let Err(e) = prepare_source(run, &dir) {
                return Outcome::Failed(format!("checkout failed: {e}"));
            }
            *g = Some(dir.clone());
        }
        dir
    } else {
        let dir = run.dir.join("jobs").join(job_dir_name(&job.id)).join(&project);
        log(&format!("Checking out {} @ {} {}", run.trig.ref_name, run.trig.short_sha, if run.trig.working { "(with your uncommitted changes)" } else { "(clean clone)" }));
        if let Err(e) = prepare_source(run, &dir) {
            return Outcome::Failed(format!("checkout failed: {e}"));
        }
        dir
    };
    let tmp_host = run.dir.join("tmp").join(format!("{idx}"));
    let _ = std::fs::create_dir_all(&tmp_host);

    let docker = rt.image.is_some();
    let ws_in = if !docker {
        ws_host.to_string_lossy().replace('\\', "/")
    } else {
        match provider {
            "gitlab" => format!("/builds/{}/{}", run.trig.namespace, project),
            "github" => format!("/home/runner/work/{0}/{0}", project),
            _ => format!("/var/jenkins_home/workspace/{project}"),
        }
    };
    let tmp_in = if docker { "/dwci".to_string() } else { tmp_host.to_string_lossy().replace('\\', "/") };

    // ---- container + services ----
    let mut td = Teardown { docker, containers: vec![], network: None };
    let cname = container_name(run, idx);
    let mut sb = Sandbox { container: None, ws_host: ws_host.clone(), ws_in: ws_in.clone(), tmp_host: tmp_host.clone(), tmp_in, has_bash: true, counter: Mutex::new(0) };

    // Base environment (provider specific). GitHub resolves env per step, so it only gets the fixed part here.
    let mut env: Vec<Kv> = match provider {
        "gitlab" => gitlab_job_env(run, idx, &ws_in),
        "github" => super::gh::job_env(run, idx, &ws_in, &sb.tmp_in),
        _ => {
            let mut e = jenkins_base_env(run, job, &ws_in);
            let params = jenkins_params(run);
            for p in &params {
                put(&mut e, &p.key, p.value.clone());
            }
            for v in &run.ci_vars {
                if !v.secret {
                    put(&mut e, &v.key, v.value.clone());
                }
            }
            jenkins_resolve_env(run, &mut e, &params, &run.pipeline.env);
            jenkins_resolve_env(run, &mut e, &params, &job.env);
            e
        }
    };
    put(&mut env, "GIT_CONFIG_COUNT", "1");
    put(&mut env, "GIT_CONFIG_KEY_0", "safe.directory");
    put(&mut env, "GIT_CONFIG_VALUE_0", "*");

    if let Some(image) = &rt.image {
        let mut emit = |l: &str| run.log(idx, -1, l);
        if let Err(e) = exec::ensure_image(image, &run.cancel, &mut emit) {
            return Outcome::Failed(e);
        }
        let network = if job.services.is_empty() { None } else { Some(format!("{cname}-net")) };
        if let Some(n) = &network {
            exec::remove_network(n);
            if let Err(e) = exec::create_network(n) {
                return Outcome::Failed(format!("could not create the job network: {e}"));
            }
            td.network = Some(n.clone());
        }
        // Services
        for (k, svc) in job.services.iter().enumerate() {
            let svc_image = if provider == "github" { super::gh::interp_job(run, idx, &svc.image) } else { expand_vars(&svc.image, &env) };
            if let Err(e) = exec::ensure_image(&svc_image, &run.cancel, &mut emit) {
                return Outcome::Failed(format!("service `{}`: {e}", svc.alias));
            }
            let sname = format!("{cname}-svc{k}");
            let mut senv: Vec<Kv> = vec![];
            if provider == "gitlab" {
                for kv in &env {
                    if job.env.iter().any(|j| j.key == kv.key) || run.pipeline.env.iter().any(|j| j.key == kv.key) {
                        put(&mut senv, &kv.key, kv.value.clone());
                    }
                }
            }
            for kv in &svc.env {
                let v = if provider == "github" { super::gh::interp_job(run, idx, &kv.value) } else { expand_vars(&kv.value, &env) };
                put(&mut senv, &kv.key, v);
            }
            let dind = svc_image.contains("dind");
            if dind && get(&senv, "DOCKER_TLS_CERTDIR").is_none() {
                put(&mut senv, "DOCKER_TLS_CERTDIR", "");
            }
            let aliases = {
                let mut a = vec![svc.alias.clone()];
                if provider == "gitlab" {
                    let base = svc_image.split(':').next().unwrap_or("").to_string();
                    let full = base.replace('/', "__");
                    let dashed = base.replace('/', "-");
                    for x in [full, dashed] {
                        if !x.is_empty() && !a.contains(&x) {
                            a.push(x);
                        }
                    }
                }
                a
            };
            let mut args: Vec<String> = svc.options.iter().map(|o| super::gh::interp_job(run, idx, o)).collect();
            // On the host (no job container) services have to be reachable on localhost.
            let mut note_ports = false;
            if false {
                note_ports = true;
            }
            let _ = note_ports;
            let _ = &mut args;
            emit(&format!("Starting service {} ({})", svc.alias, svc_image));
            let spec = exec::ContainerSpec {
                name: &sname,
                image: &svc_image,
                network: network.as_deref(),
                aliases,
                mounts: vec![],
                workdir: None,
                env: senv,
                args,
                cmd: svc.command.clone().map(|c| c.split_whitespace().map(String::from).collect()).unwrap_or_default(),
                keep_alive: false,
                privileged: dind,
            };
            td.containers.push(sname.clone());
            if let Err(e) = exec::start_container(&spec) {
                return Outcome::Failed(format!("service `{}` failed to start: {e}", svc.alias));
            }
        }

        // Job container
        let mut mounts: Vec<(String, String, bool)> = vec![
            (ws_host.to_string_lossy().into_owned(), ws_in.clone(), false),
            (tmp_host.to_string_lossy().into_owned(), "/dwci".into(), false),
        ];
        let tools = run.ci_dir.join("tools");
        let _ = std::fs::create_dir_all(&tools);
        mounts.push((tools.to_string_lossy().into_owned(), "/opt/hostedtoolcache".into(), false));
        let actions = run.ci_dir.join("actions");
        let _ = std::fs::create_dir_all(&actions);
        mounts.push((actions.to_string_lossy().into_owned(), "/dwci-actions".into(), true));
        let cache_dir = run.ci_dir.join("gh-cache");
        let _ = std::fs::create_dir_all(&cache_dir);
        mounts.push((cache_dir.to_string_lossy().into_owned(), "/dwci-cache".into(), false));
        if provider == "github" {
            mounts.push(("/var/run/docker.sock".into(), "/var/run/docker.sock".into(), false));
        }
        let spec = exec::ContainerSpec {
            name: &cname,
            image: rt.image.as_deref().unwrap(),
            network: network.as_deref(),
            aliases: vec![],
            mounts,
            workdir: Some(&ws_in),
            env: vec![],
            args: job.docker_args.clone(),
            cmd: vec![],
            keep_alive: true,
            privileged: false,
        };
        exec::remove_container(&cname);
        // Registered first so a half-created container is removed even when starting it fails.
        td.containers.push(cname.clone());
        if let Err(e) = exec::start_container(&spec) {
            return Outcome::Failed(format!("could not start the job container: {e}"));
        }
        sb.container = Some(cname.clone());
        sb.has_bash = exec::detect_bash(&cname);

        // Wait for services
        for (k, svc) in job.services.iter().enumerate() {
            let sname = format!("{cname}-svc{k}");
            wait_service(run, idx, &sb, &sname, svc, &rt.image.clone().unwrap_or_default());
        }
        let _ = emit;
    } else {
        sb.has_bash = exec::host_bash().is_some();
        if provider != "jenkins" && !job.services.is_empty() {
            log("note: services need Docker; they are not started when running on this machine");
        }
    }

    let timeout = job.timeout_secs.or(if provider == "gitlab" { Some(3600) } else { None });
    let ctx = JobCtx {
        run,
        idx,
        job,
        sb,
        env,
        masks: Mutex::new(run.mask_values()),
        deadline: timeout.map(|s| Instant::now() + Duration::from_secs(s)),
        cur: Cell::new(-1),
        image: rt.image.clone(),
        sources: RefCell::new(vec![]),
    };
    log(&format!("Job ready in {}", fmt_dur(t0.elapsed().as_millis())));
    let out = ctx.execute();
    let _ = td;
    out
}

fn wait_service(run: &Run, idx: usize, sb: &Sandbox, sname: &str, svc: &ServiceDef, _job_image: &str) {
    let log = |t: &str| run.log(idx, -1, t);
    let started = Instant::now();
    // Healthcheck-defined services: wait for "healthy".
    loop {
        if run.cancel.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(120) {
            return;
        }
        if !exec::container_running(sname) {
            log(&format!("service {} is not running (it may have exited); continuing", svc.alias));
            return;
        }
        match exec::health_status(sname).as_deref() {
            Some("starting") => std::thread::sleep(Duration::from_secs(1)),
            Some("unhealthy") => {
                log(&format!("service {} reports unhealthy", svc.alias));
                return;
            }
            Some(_) => {
                log(&format!("service {} is healthy", svc.alias));
                return;
            }
            None => break,
        }
    }
    // No healthcheck: wait for its first exposed port to accept connections from the job container.
    let image = svc.image.clone();
    let ports = exec::exposed_ports(&image);
    let Some(port) = ports.first().copied() else {
        std::thread::sleep(Duration::from_secs(2));
        return;
    };
    let probe = format!("(command -v nc >/dev/null 2>&1 && nc -z {a} {p}) || (command -v bash >/dev/null 2>&1 && bash -c 'echo > /dev/tcp/{a}/{p}') >/dev/null 2>&1", a = svc.alias, p = port);
    let t = Instant::now();
    loop {
        if run.cancel.load(Ordering::SeqCst) || t.elapsed() > Duration::from_secs(45) {
            log(&format!("service {} did not open port {port} in time; continuing", svc.alias));
            return;
        }
        if !exec::container_running(sname) {
            return;
        }
        let ok = sb.quick(&["sh", "-c", &probe]).is_some();
        if ok {
            log(&format!("service {} is accepting connections on port {port}", svc.alias));
            return;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

// ============================== executing ==============================

impl<'a> JobCtx<'a> {
    pub fn emit(&self, text: &str) {
        let mut t = text.to_string();
        for m in self.masks.lock().unwrap().iter() {
            if !m.is_empty() && t.contains(m.as_str()) {
                t = t.replace(m.as_str(), "***");
            }
        }
        self.run.log(self.idx, self.cur.get(), &t);
    }

    pub fn provider(&self) -> &str {
        &self.run.pipeline.provider
    }

    fn cancelled(&self) -> bool {
        self.run.cancel.load(Ordering::SeqCst)
    }

    fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(Instant::now()))
    }

    fn set_step(&self, ui: usize, status: &str) {
        let mut d = self.run.jobs[self.idx].dynamic.lock().unwrap();
        if let Some(s) = d.steps.get_mut(ui) {
            if status == "running" {
                s.started_ms = Some(now_ms());
            } else {
                s.finished_ms = Some(now_ms());
            }
            s.status = status.to_string();
        }
    }

    pub fn execute(&self) -> Outcome {
        let provider = self.provider().to_string();
        let mut st = St {
            failed: false,
            fail_msg: None,
            cancelled: false,
            unstable: false,
            catch_failed: false,
            gh_env: vec![],
            path_add: vec![],
            scopes: vec![GhScope { inputs: super::gh::workflow_inputs(self.run), steps: Map::new(), action_path: String::new() }],
            post: vec![],
            jvars: HashMap::new(),
            jenv: vec![],
            step_files: 0,
        };

        if provider == "gitlab" {
            self.gl_restore(&mut st);
        }
        if provider == "github" {
            super::gh::start_job(self, &mut st);
        }

        self.run_steps(&self.job.steps, &mut st, None);

        // Provider epilogue
        match provider.as_str() {
            "gitlab" => self.gl_finish(&st),
            "github" => super::gh::finish_job(self, &mut st),
            _ => self.jenkins_post(&mut st),
        }

        if self.cancelled() || st.cancelled {
            return Outcome::Canceled;
        }
        if st.failed {
            let msg = st.fail_msg.clone().unwrap_or_else(|| "a step failed".into());
            if provider == "gitlab" {
                self.emit(&format!("ERROR: Job failed: {msg}"));
            }
            return Outcome::Failed(msg);
        }
        if provider == "gitlab" {
            self.emit("Job succeeded");
        }
        if st.unstable {
            *self.run.build_result.lock().unwrap() = "UNSTABLE".into();
            return Outcome::Warning("build is UNSTABLE".into());
        }
        if st.catch_failed {
            *self.run.build_result.lock().unwrap() = "FAILURE".into();
            return Outcome::Warning("a step failed inside catchError (the build is marked FAILURE but continued)".into());
        }
        Outcome::Passed
    }

    pub fn run_steps(&self, steps: &[Step], st: &mut St, ui: Option<usize>) {
        let only: Vec<usize> = if ui.is_none() { self.run.jobs[self.idx].dynamic.lock().unwrap().only_steps.clone() } else { vec![] };
        for (i, step) in steps.iter().enumerate() {
            let idx = ui.unwrap_or(i);
            let top = ui.is_none();
            self.cur.set(idx as i32);
            if top && !only.is_empty() && !only.contains(&i) {
                self.set_step(idx, "skipped");
                continue;
            }
            if top {
                self.set_step(idx, "running");
            }
            let res = self.run_step(step, st, idx);
            if top {
                let s = match &res {
                    StepRes::Ok => "passed",
                    StepRes::Failed(_) => "failed",
                    StepRes::Skipped(_) => "skipped",
                    StepRes::Cancelled => "canceled",
                };
                self.set_step(idx, s);
            }
            self.cur.set(idx as i32);
        }
    }

    fn should_run(&self, step: &Step, st: &St) -> Result<bool, String> {
        if self.cancelled() {
            return Ok(false);
        }
        if self.provider() == "github" {
            return match &step.cond {
                Some(c) => super::gh::step_condition(self, st, c),
                None => Ok(!st.failed),
            };
        }
        Ok(!st.failed || step.always)
    }

    fn run_step(&self, step: &Step, st: &mut St, ui: usize) -> StepRes {
        match self.should_run(step, st) {
            Ok(true) => {}
            Ok(false) => {
                if self.cancelled() {
                    st.cancelled = true;
                    return StepRes::Cancelled;
                }
                return StepRes::Skipped("skipped".into());
            }
            Err(e) => {
                self.emit(&format!("Error evaluating condition: {e}"));
                st.failed = true;
                st.fail_msg = Some(e.clone());
                return StepRes::Failed(e);
            }
        }
        let mut res = StepRes::Ok;
        let tries = step.retry + 1;
        for attempt in 0..tries {
            res = self.exec_kind(step, st, ui);
            match &res {
                StepRes::Failed(m) if attempt + 1 < tries => {
                    self.emit(&format!("Step failed ({m}); retrying ({}/{})", attempt + 2, tries));
                    continue;
                }
                _ => break,
            }
        }
        match &res {
            StepRes::Failed(m) => {
                if self.provider() == "github" {
                    super::gh::record_failure(self, st, step);
                }
                if step.continue_on_error {
                    self.emit(&format!("(step failed but `continue-on-error` is set: {m})"));
                } else if step.catch_error {
                    self.emit(&format!("catchError: {m}; marking the build as FAILURE and continuing"));
                    st.catch_failed = true;
                } else {
                    st.failed = true;
                    if st.fail_msg.is_none() {
                        st.fail_msg = Some(m.clone());
                    }
                }
            }
            StepRes::Cancelled => st.cancelled = true,
            _ => {}
        }
        res
    }

    fn step_timeout(&self, step: &Step) -> Option<Duration> {
        let own = step.timeout_secs.map(Duration::from_secs);
        match (own, self.remaining()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn dir_for(&self, step: &Step) -> String {
        let d = step.dir.clone().or_else(|| self.job.default_dir.clone()).unwrap_or_default();
        let d = if self.provider() == "github" { super::gh::interp_job(self.run, self.idx, &d) } else { d };
        let d = d.replace('\\', "/");
        if d.is_empty() || d == "." {
            return self.sb.ws_in.clone();
        }
        if d.starts_with('/') || d.chars().nth(1) == Some(':') {
            return d;
        }
        // `dir()` in Jenkins creates missing directories.
        let _ = std::fs::create_dir_all(self.sb.ws_host.join(&d));
        format!("{}/{}", self.sb.ws_in.trim_end_matches('/'), d.trim_start_matches("./"))
    }

    pub fn jscope_env(&self, st: &St) -> Vec<Kv> {
        let mut e = self.env.clone();
        for kv in &st.jenv {
            put(&mut e, &kv.key, kv.value.clone());
        }
        e
    }

    fn jtext_now(&self, raw: &str, st: &St) -> String {
        let env = self.jscope_env(st);
        let params = jenkins_params(self.run);
        let sc = JScope { env: &env, params: &params, vars: &st.jvars, run: self.run, result: self.run.build_result.lock().unwrap().clone(), ws: Some(&self.sb.ws_host) };
        jtext(raw, &sc, self.run)
    }

    fn exec_kind(&self, step: &Step, st: &mut St, ui: usize) -> StepRes {
        match &step.kind {
            StepKind::Script { script, style, interpolate } => self.run_script(step, script, style, *interpolate, st),
            StepKind::Uses { action, with } => super::gh::run_uses(self, step, action, with, st),
            StepKind::Echo { text, interpolate } => {
                let t = if self.provider() == "jenkins" {
                    if *interpolate && !text.starts_with(EXPR) && !text.starts_with(GSTR) {
                        self.jtext_now(&format!("{GSTR}{text}"), st)
                    } else {
                        self.jtext_now(text, st)
                    }
                } else {
                    text.clone()
                };
                self.emit(&t);
                StepRes::Ok
            }
            StepKind::Archive { patterns, allow_empty } => {
                let dest = self.run.dir.join("artifacts").join(format!("{}", self.idx));
                let pats: Vec<String> = patterns.split(',').map(|s| s.trim().to_string()).collect();
                let files = collect_files(&self.sb.ws_host, &pats, &[]);
                if files.is_empty() {
                    if *allow_empty {
                        self.emit(&format!("No files matched `{patterns}` (allowEmptyArchive)"));
                        return StepRes::Ok;
                    }
                    return StepRes::Failed(format!("archiveArtifacts: no files match `{patterns}`"));
                }
                let n = copy_files(&self.sb.ws_host, &dest, &files);
                self.emit(&format!("Archiving artifacts: {n} file(s)"));
                self.run.jobs[self.idx].dynamic.lock().unwrap().artifacts.extend(files.iter().map(|f| f.to_string_lossy().replace('\\', "/")));
                StepRes::Ok
            }
            StepKind::Stash { name, includes } => {
                let dest = self.run.dir.join("stash").join(name);
                let _ = std::fs::remove_dir_all(&dest);
                let pats: Vec<String> = includes.split(',').map(|s| s.trim().to_string()).collect();
                let files = collect_files(&self.sb.ws_host, &pats, &[]);
                let n = copy_files(&self.sb.ws_host, &dest, &files);
                self.emit(&format!("Stashed {n} file(s) to `{name}`"));
                StepRes::Ok
            }
            StepKind::Unstash { name } => {
                let src = self.run.dir.join("stash").join(name);
                if !src.exists() {
                    return StepRes::Failed(format!("No such saved stash `{name}`"));
                }
                let n = copy_dir_all(&src, &self.sb.ws_host);
                self.emit(&format!("Restored {n} file(s) from stash `{name}`"));
                StepRes::Ok
            }
            StepKind::Junit { pattern } => {
                let files = collect_files(&self.sb.ws_host, &[pattern.clone()], &[]);
                let (mut tests, mut failures) = (0, 0);
                for f in &files {
                    if let Ok(t) = std::fs::read_to_string(self.sb.ws_host.join(f)) {
                        tests += t.matches("<testcase").count();
                        failures += t.matches("<failure").count() + t.matches("<error").count();
                    }
                }
                self.emit(&format!("Recording test results: {} report(s), {tests} test(s), {failures} failure(s)", files.len()));
                if failures > 0 {
                    st.unstable = true;
                }
                StepRes::Ok
            }
            StepKind::Sleep { seconds } => {
                let end = Instant::now() + Duration::from_secs(*seconds);
                self.emit(&format!("Sleeping for {seconds} sec"));
                while Instant::now() < end {
                    if self.cancelled() {
                        return StepRes::Cancelled;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                StepRes::Ok
            }
            StepKind::Error { message } => {
                let m = self.jtext_now(message, st);
                self.emit(&format!("ERROR: {m}"));
                StepRes::Failed(m)
            }
            StepKind::Input { message } => {
                self.emit(&format!("Input requested: {message}  (auto-approved by the simulator)"));
                StepRes::Ok
            }
            StepKind::If { cond, then, otherwise } => {
                let env = self.jscope_env(st);
                let params = jenkins_params(self.run);
                let truth = {
                    let sc = JScope { env: &env, params: &params, vars: &st.jvars, run: self.run, result: self.run.build_result.lock().unwrap().clone(), ws: Some(&self.sb.ws_host) };
                    groovy::eval(cond, &sc)
                };
                match truth {
                    Ok(v) => {
                        let branch = if v.truthy() { then } else { otherwise };
                        self.run_steps(branch, st, Some(ui));
                        StepRes::Ok
                    }
                    Err(e) => {
                        self.emit(&format!("Cannot evaluate `if ({cond})`: {e}"));
                        StepRes::Failed(format!("cannot evaluate condition `{cond}`: {e}"))
                    }
                }
            }
            StepKind::Try { body, catch_, finally } => {
                let before = st.failed;
                st.failed = false;
                self.run_steps(body, st, Some(ui));
                if st.failed {
                    self.emit("catch: the try block failed");
                    st.failed = false;
                    st.fail_msg = None;
                    self.run_steps(catch_, st, Some(ui));
                }
                let after_catch = st.failed;
                let fm = st.fail_msg.clone();
                st.failed = false;
                self.run_steps(finally, st, Some(ui));
                st.failed = before || after_catch || st.failed;
                if st.failed && st.fail_msg.is_none() {
                    st.fail_msg = fm;
                }
                StepRes::Ok
            }
            StepKind::SetVar { key, expr, script, env } => {
                let value = if let Some(s) = script {
                    let text = self.jtext_now(s, st);
                    let mut out = String::new();
                    let env_now = self.jscope_env(st);
                    let _ = self.sb.exec(&text, &Shell::JenkinsSh, &env_now, &self.sb.ws_in, &self.run.cancel, self.step_timeout(&Step::new("capture", StepKind::Sleep { seconds: 0 })), &mut |l| {
                        if !l.starts_with("+ ") {
                            out.push_str(l);
                            out.push('\n');
                        }
                    });
                    out.trim().to_string()
                } else {
                    self.jtext_now(if expr.starts_with(EXPR) || expr.starts_with(GSTR) { expr } else { expr }, st)
                };
                if key == "currentBuild.result" {
                    let v = value.to_uppercase();
                    self.emit(&format!("currentBuild.result = {v}"));
                    if v == "UNSTABLE" {
                        st.unstable = true;
                    } else if v == "FAILURE" {
                        st.catch_failed = true;
                    }
                } else if *env {
                    put(&mut st.jenv, key, value);
                } else {
                    st.jvars.insert(key.clone(), value);
                }
                StepRes::Ok
            }
            StepKind::Parallel { branches } => {
                self.emit("note: `parallel` inside a script block runs its branches one after another in the simulator");
                for (name, steps) in branches {
                    self.emit(&format!("[Branch: {name}]"));
                    self.run_steps(steps, st, Some(ui));
                }
                StepRes::Ok
            }
            StepKind::Unsupported { what } => {
                self.emit(&format!("The simulator cannot run this step: {what}"));
                StepRes::Failed(format!("unsupported step: {}", what.lines().next().unwrap_or("")))
            }
        }
    }

    // ---------- scripts ----------

    fn run_script(&self, step: &Step, script: &str, style: &str, interpolate: bool, st: &mut St) -> StepRes {
        let provider = self.provider().to_string();
        let mut text: String;
        let mut env = self.env.clone();
        let mut shell: Shell;
        let mut wf = false;
        match provider.as_str() {
            "github" => {
                match super::gh::interp_step(self, st, script) {
                    Ok(t) => text = t,
                    Err(e) => {
                        self.emit(&format!("Error: {e}"));
                        return StepRes::Failed(e);
                    }
                }
                env = super::gh::step_env(self, st, step);
                let name = step.shell.clone().or_else(|| self.job.default_shell.clone()).map(|s| super::gh::interp_job(self.run, self.idx, &s));
                shell = match name {
                    Some(n) => Shell::parse(&n),
                    None => {
                        if self.sb.has_bash {
                            Shell::Bash
                        } else {
                            Shell::Sh
                        }
                    }
                };
                if matches!(shell, Shell::Bash) && !self.sb.has_bash {
                    shell = Shell::Sh;
                }
                self.emit(&format!("Run {}", text.trim().lines().next().unwrap_or("")));
                if text.trim().lines().count() > 1 {
                    for l in text.trim().lines().skip(1) {
                        self.emit(&format!("  {l}"));
                    }
                }
                wf = true;
            }
            "gitlab" => {
                for kv in &step.env {
                    put(&mut env, &kv.key, kv.value.clone());
                }
                let bash = self.sb.has_bash;
                let mut s = String::new();
                s.push_str(if bash { "set -eo pipefail\n" } else { "set -e\n" });
                for entry in script.split(SEP) {
                    let entry = entry.trim_end();
                    if entry.trim().is_empty() {
                        continue;
                    }
                    s.push_str(&format!("printf '%s\\n' {}\n", exec::single_quote(&format!("$ {entry}"))));
                    s.push_str(entry);
                    s.push('\n');
                }
                text = s;
                shell = Shell::Auto;
                if step.name == "after_script" {
                    // after_script has its own shell and its failure does not fail the job
                    text = text.replacen("set -eo pipefail\n", "", 1).replacen("set -e\n", "", 1);
                }
            }
            _ => {
                // Jenkins
                text = self.jtext_now(&if interpolate && !script.starts_with(EXPR) && !script.starts_with(GSTR) { format!("{GSTR}{script}") } else { script.to_string() }, st);
                for kv in &st.jenv {
                    put(&mut env, &kv.key, kv.value.clone());
                }
                for kv in &step.env {
                    let v = self.jtext_now(&kv.value, st);
                    put(&mut env, &kv.key, v);
                }
                for kv in &mut env {
                    if kv.value.starts_with(CRED) || kv.value.starts_with(GSTR) || kv.value.starts_with(EXPR) {
                        kv.value = self.jtext_now(&kv.value, st);
                    }
                }
                for kv in &env {
                    if step.env.iter().any(|e| e.key == kv.key) && kv.value.len() >= 3 && self.job_secret_var(&step.env, &kv.key) {
                        self.masks.lock().unwrap().push(kv.value.clone());
                    }
                }
                self.emit(&format!("[Pipeline] {}", match style {
                    "bat" => "bat",
                    "pwsh" => "pwsh",
                    _ => "sh",
                }));
                shell = match style {
                    "bat" => Shell::Cmd,
                    "pwsh" => Shell::Pwsh,
                    _ => Shell::JenkinsSh,
                };
                if style == "bat" && self.sb.is_docker() {
                    let m = "`bat` needs a Windows agent; this stage runs in a Linux container".to_string();
                    self.emit(&m);
                    return StepRes::Failed(m);
                }
            }
        }
        if !st.path_add.is_empty() && matches!(shell, Shell::Bash | Shell::Sh | Shell::Auto | Shell::JenkinsSh) {
            text = format!("export PATH=\"{}:$PATH\"\n{text}", st.path_add.join(":"));
        }
        let dir = self.dir_for(step);
        let timeout = self.step_timeout(step);
        let mut cap = Captured::default();
        let res = self.exec_captured(&text, &shell, &env, &dir, timeout, wf, &mut cap);
        if provider == "github" {
            super::gh::absorb(self, st, step, cap, matches!(&res, Ok(0)));
        }
        match res {
            Ok(0) => StepRes::Ok,
            Ok(code) => {
                if provider == "gitlab" && step.name == "after_script" {
                    self.emit(&format!("WARNING: after_script failed, but job will continue unaffected: exit code {code}"));
                    return StepRes::Ok;
                }
                StepRes::Failed(format!("exit code {code}"))
            }
            Err(e) if e == "cancelled" => StepRes::Cancelled,
            Err(e) if e == "timeout" => {
                self.emit("ERROR: step timed out");
                StepRes::Failed("timed out".into())
            }
            Err(e) => {
                self.emit(&format!("ERROR: {e}"));
                StepRes::Failed(e)
            }
        }
    }

    fn job_secret_var(&self, _env: &[Kv], _key: &str) -> bool {
        false
    }

    /// Execute a script; GitHub workflow commands (`::set-output` ...) are interpreted when `wf` is set.
    pub fn exec_captured(&self, text: &str, shell: &Shell, env: &[Kv], dir: &str, timeout: Option<Duration>, wf: bool, cap: &mut Captured) -> Result<i32, String> {
        let mut emit = |l: &str| {
            if wf && l.starts_with("::") {
                if let Some(rest) = l.strip_prefix("::") {
                    let (cmd, msg) = rest.split_once("::").unwrap_or((rest, ""));
                    let (name, props) = cmd.split_once(' ').unwrap_or((cmd, ""));
                    match name {
                        "add-mask" => {
                            self.masks.lock().unwrap().push(msg.to_string());
                            return;
                        }
                        "error" | "warning" | "notice" => {
                            self.emit(&format!("{}: {}", name.to_uppercase(), msg));
                            return;
                        }
                        "debug" | "endgroup" => return,
                        "group" => {
                            self.emit(&format!("> {msg}"));
                            return;
                        }
                        "set-output" => {
                            if let Some(n) = props.strip_prefix("name=") {
                                cap.outputs.push(Kv::new(n, msg));
                            }
                            return;
                        }
                        "set-env" => {
                            if let Some(n) = props.strip_prefix("name=") {
                                cap.env.push(Kv::new(n, msg));
                            }
                            return;
                        }
                        "add-path" => {
                            cap.paths.push(msg.to_string());
                            return;
                        }
                        _ => {}
                    }
                }
            }
            self.emit(l);
        };
        self.sb.exec(text, shell, env, dir, &self.run.cancel, timeout, &mut emit)
    }

    // ---------- GitLab ----------

    fn gl_deps(&self) -> Vec<usize> {
        let job = self.job;
        let run = self.run;
        if let Some(deps) = &job.dependencies {
            return deps.iter().flat_map(|d| run.find_jobs(d)).collect();
        }
        if job.has_needs {
            return job.needs.iter().filter(|n| n.artifacts).flat_map(|n| run.find_jobs(&n.job)).collect();
        }
        let si = run.stage_index(&job.stage);
        (0..run.jobs.len()).filter(|i| *i != self.idx && run.stage_index(&run.jobs[*i].job.stage) < si).collect()
    }

    fn gl_restore(&self, _st: &mut St) {
        let deps = self.gl_deps();
        let mut n = 0;
        let mut from = vec![];
        for d in deps {
            let src = self.run.dir.join("artifacts").join(format!("{d}"));
            if src.exists() {
                n += copy_dir_all(&src, &self.sb.ws_host);
                from.push(self.run.jobs[d].job.name.clone());
            }
        }
        if n > 0 {
            self.emit(&format!("Downloading artifacts from {}: {n} file(s)", from.join(", ")));
        }
        *self.sources.borrow_mut() = vec![];
        // Cache
        for c in &self.job.cache {
            if c.policy == "push" {
                continue;
            }
            let dir = self.gl_cache_dir(c);
            if dir.exists() {
                let k = copy_dir_all(&dir, &self.sb.ws_host);
                self.emit(&format!("Restoring cache `{}`: {k} file(s)", c.key));
            } else {
                self.emit(&format!("Cache `{}` not found (cache miss)", c.key));
            }
        }
    }

    fn gl_cache_dir(&self, c: &CacheDef) -> PathBuf {
        let key = if let Some(rest) = c.key.strip_prefix("files:") {
            let (files, prefix) = rest.split_once(";prefix:").unwrap_or((rest, ""));
            let list: Vec<String> = files.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
            let h = hash_files(&self.sb.ws_host, &list);
            if prefix.is_empty() { h } else { format!("{prefix}-{h}") }
        } else {
            expand_vars(&c.key, &self.env)
        };
        self.run.ci_dir.join("cache").join(slug(&self.run.trig.project_name)).join(slug(&key))
    }

    fn gl_finish(&self, st: &St) {
        let job = self.job;
        let ok = !st.failed && !st.cancelled;
        if let Some(a) = &job.artifacts {
            let upload = match a.when.as_str() {
                "always" => true,
                "on_failure" => !ok,
                _ => ok,
            };
            if upload && !self.cancelled() {
                let mut paths = a.paths.clone();
                if let Some(d) = &a.dotenv {
                    paths.push(d.clone());
                }
                let files = collect_files(&self.sb.ws_host, &paths, &a.exclude);
                if files.is_empty() {
                    if !a.paths.is_empty() {
                        self.emit("WARNING: no files to upload for artifacts:paths");
                    }
                } else {
                    let dest = self.run.dir.join("artifacts").join(format!("{}", self.idx));
                    let n = copy_files(&self.sb.ws_host, &dest, &files);
                    self.emit(&format!("Uploading artifacts: {n} file(s)"));
                    self.run.jobs[self.idx].dynamic.lock().unwrap().artifacts = files.iter().map(|f| f.to_string_lossy().replace('\\', "/")).collect();
                }
                if let Some(d) = &a.dotenv {
                    if let Ok(t) = std::fs::read_to_string(self.sb.ws_host.join(d)) {
                        self.run.jobs[self.idx].dynamic.lock().unwrap().dotenv = parse_dotenv(&t);
                    }
                }
            }
        }
        if ok {
            for c in &job.cache {
                if c.policy == "pull" {
                    continue;
                }
                let dir = self.gl_cache_dir(c);
                let files = collect_files(&self.sb.ws_host, &c.paths, &[]);
                let _ = std::fs::remove_dir_all(&dir);
                let n = copy_files(&self.sb.ws_host, &dir, &files);
                self.emit(&format!("Saving cache `{}`: {n} file(s)", c.key));
            }
        }
        if let Some(rx) = &job.coverage {
            let rx = rx.trim().trim_start_matches('/').trim_end_matches('/');
            if let Ok(re) = regex::Regex::new(rx) {
                let log = self.run.jobs[self.idx].log.lock().unwrap();
                let mut cov = None;
                for l in log.iter() {
                    if let Some(c) = re.captures(&l.text) {
                        cov = c.get(1).or_else(|| c.get(0)).map(|m| m.as_str().to_string());
                    }
                }
                drop(log);
                if let Some(c) = cov {
                    let digits: String = c.chars().filter(|x| x.is_ascii_digit() || *x == '.').collect();
                    self.run.jobs[self.idx].dynamic.lock().unwrap().coverage = Some(digits);
                }
            }
        }
    }

    // ---------- Jenkins ----------

    fn jenkins_post(&self, st: &mut St) {
        if self.job.post.is_empty() {
            return;
        }
        let is_final = self.job.id == "post actions";
        let result = if self.cancelled() {
            "ABORTED".to_string()
        } else if is_final {
            let r = self.run.build_result.lock().unwrap().clone();
            let any_failed = self.run.jobs.iter().any(|j| j.dynamic.lock().unwrap().status == "failed");
            if any_failed {
                "FAILURE".to_string()
            } else {
                r
            }
        } else if st.failed {
            "FAILURE".to_string()
        } else if st.unstable {
            "UNSTABLE".to_string()
        } else {
            "SUCCESS".to_string()
        };
        let order = ["always", "changed", "fixed", "regression", "aborted", "failure", "success", "unstable", "unsuccessful", "cleanup"];
        for cond in order {
            for b in self.job.post.iter().filter(|b| b.cond == cond) {
                let hit = match cond {
                    "always" | "cleanup" => true,
                    "success" => result == "SUCCESS",
                    "failure" => result == "FAILURE",
                    "unstable" => result == "UNSTABLE",
                    "unsuccessful" => result != "SUCCESS",
                    "aborted" => result == "ABORTED",
                    _ => false,
                };
                if !hit {
                    continue;
                }
                self.emit(&format!("[Pipeline] post {cond}"));
                let was_failed = st.failed;
                st.failed = false;
                self.run_post_steps(&b.steps, st);
                if st.failed {
                    // keep the failure
                } else {
                    st.failed = was_failed;
                }
            }
        }
    }

    fn run_post_steps(&self, steps: &[Step], st: &mut St) {
        // Steps in post blocks run regardless of earlier failures; they are not tracked as top-level UI steps.
        self.cur.set(-1);
        self.run_steps(steps, st, Some(usize::MAX));
        self.cur.set(-1);
    }
}

#[derive(Default)]
pub struct Captured {
    pub outputs: Vec<Kv>,
    pub env: Vec<Kv>,
    pub paths: Vec<String>,
}

// ============================== GitHub condition entry points ==============================

pub fn gh_job_condition(run: &Run, idx: usize, cond: &str, ok: bool) -> Result<bool, String> {
    super::gh::job_condition(run, idx, cond, ok)
}

