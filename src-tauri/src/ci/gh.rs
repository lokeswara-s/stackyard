//! GitHub Actions semantics: contexts, expressions, runner files, built-in and third-party actions.

use serde_json::{json, Map, Value};
use serde_yaml::Value as Y;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::engine::*;
use super::exec::{self, Shell};
use super::expr::{gh_condition, gh_interpolate, GhCtx};
use super::gitlab::{list_of_str, s};
use super::ir::*;
use super::runner::*;
use crate::util::{command, run as run_cmd};

fn event_name(run: &Run) -> String {
    match run.trig.event.as_str() {
        "manual" | "web" => "workflow_dispatch".into(),
        "merge_request" | "merge_request_event" => "pull_request".into(),
        "" => "push".into(),
        e => e.to_string(),
    }
}

fn repository(run: &Run) -> String {
    format!("{}/{}", run.trig.namespace, run.trig.project_name)
}

fn base_id(id: &str) -> &str {
    id.split(" (").next().unwrap_or(id)
}

fn result_name(status: &str) -> &'static str {
    match status {
        "passed" | "warning" => "success",
        "failed" => "failure",
        "canceled" => "cancelled",
        _ => "skipped",
    }
}

pub fn workflow_inputs(run: &Run) -> Value {
    let mut m = Map::new();
    for p in &run.pipeline.params {
        let v = run.opts.variables.iter().find(|k| k.key == p.name).map(|k| k.value.clone()).unwrap_or_else(|| p.default.clone());
        m.insert(p.name.clone(), if p.kind == "boolean" { json!(v == "true") } else { json!(v) });
    }
    Value::Object(m)
}

fn event_json(run: &Run) -> Value {
    let t = &run.trig;
    let repo = json!({"full_name": repository(run), "name": t.project_name, "default_branch": t.default_branch, "owner": {"login": t.namespace}, "clone_url": t.origin_url, "html_url": format!("https://github.com/{}", repository(run))});
    let reff = if t.tag.is_empty() { format!("refs/heads/{}", t.branch) } else { format!("refs/tags/{}", t.tag) };
    match event_name(run).as_str() {
        "pull_request" => json!({"action": "opened", "number": 1, "pull_request": {"number": 1, "title": t.title, "body": t.message, "head": {"ref": t.branch, "sha": t.sha}, "base": {"ref": t.default_branch}}, "repository": repo}),
        "workflow_dispatch" => json!({"inputs": workflow_inputs(run), "ref": reff, "repository": repo}),
        _ => json!({"ref": reff, "before": t.before_sha, "after": t.sha, "repository": repo, "pusher": {"name": t.author_name}, "head_commit": {"id": t.sha, "message": t.message, "timestamp": t.timestamp, "author": {"name": t.author_name, "email": t.author_email}}}),
    }
}

fn github_ctx(run: &Run, idx: usize, workspace: &str, temp: &str) -> Value {
    let t = &run.trig;
    let ev = event_name(run);
    let is_pr = ev == "pull_request";
    let reff = if is_pr { "refs/pull/1/merge".to_string() } else if t.tag.is_empty() { format!("refs/heads/{}", t.branch) } else { format!("refs/tags/{}", t.tag) };
    json!({
        "event_name": ev,
        "event": event_json(run),
        "ref": reff,
        "ref_name": t.ref_name,
        "ref_type": if t.tag.is_empty() { "branch" } else { "tag" },
        "sha": t.sha,
        "repository": repository(run),
        "repository_owner": t.namespace,
        "actor": t.author_name,
        "triggering_actor": t.author_name,
        "workspace": workspace,
        "run_id": (run.number * 1000).to_string(),
        "run_number": run.number.to_string(),
        "run_attempt": "1",
        "workflow": run.pipeline.name,
        "job": base_id(&run.jobs[idx].job.id),
        "server_url": "https://github.com",
        "api_url": "https://api.github.com",
        "graphql_url": "https://api.github.com/graphql",
        "base_ref": if is_pr { t.default_branch.clone() } else { String::new() },
        "head_ref": if is_pr { t.branch.clone() } else { String::new() },
        "token": "ghs_simulated_token",
        "event_path": format!("{temp}/event.json"),
        "action_path": "",
    })
}

fn matrix_ctx(job: &Job) -> Value {
    let mut m = Map::new();
    for kv in &job.matrix {
        m.insert(kv.key.clone(), serde_json::from_str::<Value>(&kv.value).unwrap_or_else(|_| json!(kv.value)));
    }
    Value::Object(m)
}

fn needs_ctx(run: &Run, idx: usize) -> Value {
    let mut m = Map::new();
    for d in run.deps_of(idx) {
        let j = &run.jobs[d];
        let dd = j.dynamic.lock().unwrap();
        let mut outs = Map::new();
        for kv in &dd.outputs {
            outs.insert(kv.key.clone(), json!(kv.value));
        }
        let entry = json!({"result": result_name(&dd.status), "outputs": Value::Object(outs)});
        let id = base_id(&j.job.id).to_string();
        // With a matrix the "worst" result wins.
        match m.get(&id) {
            Some(prev) if prev["result"] == "failure" => {}
            _ => {
                m.insert(id, entry);
            }
        }
    }
    Value::Object(m)
}

fn vars_ctx(run: &Run) -> Value {
    let mut m = Map::new();
    for v in run.ci_vars.iter().filter(|v| !v.secret) {
        m.insert(v.key.clone(), json!(v.value));
    }
    Value::Object(m)
}

fn secrets_ctx(run: &Run) -> Value {
    let mut m = Map::new();
    for v in run.ci_vars.iter().filter(|v| v.secret) {
        m.insert(v.key.clone(), json!(v.value));
    }
    m.insert("GITHUB_TOKEN".into(), json!("ghs_simulated_token"));
    Value::Object(m)
}

fn env_ctx(list: &[Kv]) -> Value {
    let mut m = Map::new();
    for kv in list {
        m.insert(kv.key.clone(), json!(kv.value));
    }
    Value::Object(m)
}

fn root_ctx(run: &Run, idx: usize, scopes: &[GhScope], env: &[Kv], workspace: &str, temp: &str, status: &str) -> Value {
    let job = &run.jobs[idx].job;
    let top = scopes.last().cloned().unwrap_or(GhScope { inputs: Value::Null, steps: Map::new(), action_path: String::new() });
    let mut gh = github_ctx(run, idx, workspace, temp);
    gh["action_path"] = json!(top.action_path);
    json!({
        "github": gh,
        "env": env_ctx(env),
        "vars": vars_ctx(run),
        "secrets": secrets_ctx(run),
        "matrix": matrix_ctx(job),
        "needs": needs_ctx(run, idx),
        "strategy": {"fail-fast": true, "job-index": 0, "job-total": 1},
        "job": {"status": status},
        "runner": {"os": "Linux", "arch": "X64", "name": "Stackyard Runner", "temp": format!("{temp}/tmp"), "tool_cache": "/opt/hostedtoolcache", "environment": "github-hosted"},
        "steps": Value::Object(top.steps.clone()),
        "inputs": top.inputs.clone(),
    })
}

fn no_hash(_: &[String]) -> String {
    String::new()
}

/// Evaluate `${{ }}` in `text` with the contexts available at the job level (no steps yet).
pub fn interp_job(run: &Run, idx: usize, text: &str) -> String {
    if !text.contains("${{") {
        return text.to_string();
    }
    let job = &run.jobs[idx].job;
    let mut env: Vec<Kv> = vec![];
    for kv in run.pipeline.env.iter().chain(job.env.iter()) {
        put(&mut env, &kv.key, kv.value.clone());
    }
    let scopes = vec![GhScope { inputs: workflow_inputs(run), steps: Map::new(), action_path: String::new() }];
    let root = root_ctx(run, idx, &scopes, &env, "", "", "success");
    let cx = GhCtx { root: &root, ok: true, cancelled: false, hash_files: &no_hash };
    gh_interpolate(text, &cx).unwrap_or_else(|_| text.to_string())
}

pub fn job_condition(run: &Run, idx: usize, cond: &str, ok: bool) -> Result<bool, String> {
    let job = &run.jobs[idx].job;
    let mut env: Vec<Kv> = vec![];
    for kv in run.pipeline.env.iter().chain(job.env.iter()) {
        put(&mut env, &kv.key, kv.value.clone());
    }
    let scopes = vec![GhScope { inputs: workflow_inputs(run), steps: Map::new(), action_path: String::new() }];
    let root = root_ctx(run, idx, &scopes, &env, "", "", "success");
    let cx = GhCtx { root: &root, ok, cancelled: run.cancel.load(Ordering::SeqCst), hash_files: &no_hash };
    gh_condition(cond, &cx)
}

/// The fixed environment of a GitHub job (before workflow/job/step `env`).
pub fn job_env(run: &Run, idx: usize, ws_in: &str, tmp_in: &str) -> Vec<Kv> {
    let t = &run.trig;
    let g = github_ctx(run, idx, ws_in, tmp_in);
    let mut e: Vec<Kv> = vec![];
    let mut p = |k: &str, v: &str| put(&mut e, k, v);
    p("CI", "true");
    p("GITHUB_ACTIONS", "true");
    p("GITHUB_ACTOR", &t.author_name);
    p("GITHUB_REPOSITORY", &repository(run));
    p("GITHUB_REPOSITORY_OWNER", &t.namespace);
    p("GITHUB_SHA", &t.sha);
    p("GITHUB_REF", g["ref"].as_str().unwrap_or(""));
    p("GITHUB_REF_NAME", &t.ref_name);
    p("GITHUB_REF_TYPE", g["ref_type"].as_str().unwrap_or("branch"));
    p("GITHUB_HEAD_REF", g["head_ref"].as_str().unwrap_or(""));
    p("GITHUB_BASE_REF", g["base_ref"].as_str().unwrap_or(""));
    p("GITHUB_EVENT_NAME", g["event_name"].as_str().unwrap_or("push"));
    p("GITHUB_EVENT_PATH", &format!("{tmp_in}/event.json"));
    p("GITHUB_WORKFLOW", &run.pipeline.name);
    p("GITHUB_JOB", base_id(&run.jobs[idx].job.id));
    p("GITHUB_RUN_ID", &(run.number * 1000).to_string());
    p("GITHUB_RUN_NUMBER", &run.number.to_string());
    p("GITHUB_RUN_ATTEMPT", "1");
    p("GITHUB_WORKSPACE", ws_in);
    p("GITHUB_SERVER_URL", "https://github.com");
    p("GITHUB_API_URL", "https://api.github.com");
    p("GITHUB_GRAPHQL_URL", "https://api.github.com/graphql");
    p("GITHUB_TOKEN", "ghs_simulated_token");
    p("RUNNER_OS", "Linux");
    p("RUNNER_ARCH", "X64");
    p("RUNNER_NAME", "Stackyard Runner");
    p("RUNNER_ENVIRONMENT", "github-hosted");
    p("RUNNER_TEMP", &format!("{tmp_in}/tmp"));
    p("RUNNER_TOOL_CACHE", "/opt/hostedtoolcache");
    p("RUNNER_WORKSPACE", ws_in.rsplit_once('/').map(|x| x.0).unwrap_or(ws_in));
    p("DEBIAN_FRONTEND", "noninteractive");
    e
}

fn ctx_root<'a>(ctx: &JobCtx, st: &St, env: &[Kv], status: &str) -> Value {
    root_ctx(ctx.run, ctx.idx, &st.scopes, env, &ctx.sb.ws_in, &ctx.sb.tmp_in, status)
}

fn hash_fn(ctx: &JobCtx) -> impl Fn(&[String]) -> String {
    let ws = ctx.sb.ws_host.clone();
    move |pats: &[String]| hash_files(&ws, pats)
}

/// Workflow/job level env with `${{ }}` resolved in order.
fn resolved_env(ctx: &JobCtx, st: &St, extra: &[Kv]) -> Vec<Kv> {
    let mut out: Vec<Kv> = vec![];
    let hf = hash_fn(ctx);
    let lists: [&[Kv]; 4] = [&ctx.run.pipeline.env, &ctx.job.env, &st.gh_env, extra];
    for list in lists {
        for kv in list {
            let root = ctx_root(ctx, st, &out, "success");
            let cx = GhCtx { root: &root, ok: !st.failed, cancelled: false, hash_files: &hf };
            let v = gh_interpolate(&kv.value, &cx).unwrap_or_else(|_| kv.value.clone());
            put(&mut out, &kv.key, v);
        }
    }
    out
}

pub fn interp_step(ctx: &JobCtx, st: &St, text: &str) -> Result<String, String> {
    if !text.contains("${{") {
        return Ok(text.to_string());
    }
    let env = resolved_env(ctx, st, &[]);
    let root = ctx_root(ctx, st, &env, if st.failed { "failure" } else { "success" });
    let hf = hash_fn(ctx);
    let cx = GhCtx { root: &root, ok: !st.failed, cancelled: ctx.run.cancel.load(Ordering::SeqCst), hash_files: &hf };
    gh_interpolate(text, &cx)
}

pub fn step_condition(ctx: &JobCtx, st: &St, cond: &str) -> Result<bool, String> {
    let env = resolved_env(ctx, st, &[]);
    let root = ctx_root(ctx, st, &env, if st.failed { "failure" } else { "success" });
    let hf = hash_fn(ctx);
    let cx = GhCtx { root: &root, ok: !st.failed, cancelled: ctx.run.cancel.load(Ordering::SeqCst), hash_files: &hf };
    gh_condition(cond, &cx)
}

/// Create the runner command files for the next step and return the full step environment.
pub fn step_env(ctx: &JobCtx, st: &mut St, step: &Step) -> Vec<Kv> {
    st.step_files += 1;
    let n = st.step_files;
    let host_dir = ctx.sb.tmp_host.join("files").join(n.to_string());
    let _ = std::fs::create_dir_all(&host_dir);
    for f in ["env", "output", "path", "summary", "state"] {
        let _ = std::fs::write(host_dir.join(f), "");
    }
    let _ = std::fs::create_dir_all(ctx.sb.tmp_host.join("tmp"));
    let _ = std::fs::write(ctx.sb.tmp_host.join("event.json"), event_json(ctx.run).to_string());
    let mut env = ctx.env.clone();
    for kv in resolved_env(ctx, st, &step.env) {
        put(&mut env, &kv.key, kv.value);
    }
    let base = format!("{}/files/{n}", ctx.sb.tmp_in.trim_end_matches('/'));
    put(&mut env, "GITHUB_ENV", format!("{base}/env"));
    put(&mut env, "GITHUB_OUTPUT", format!("{base}/output"));
    put(&mut env, "GITHUB_PATH", format!("{base}/path"));
    put(&mut env, "GITHUB_STEP_SUMMARY", format!("{base}/summary"));
    put(&mut env, "GITHUB_STATE", format!("{base}/state"));
    if let Some(sc) = st.scopes.last() {
        if !sc.action_path.is_empty() {
            put(&mut env, "GITHUB_ACTION_PATH", sc.action_path.clone());
        }
    }
    env
}

/// `KEY=value` and `KEY<<DELIM ... DELIM` files written by steps.
fn parse_command_file(text: &str) -> Vec<Kv> {
    let mut out = vec![];
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        i += 1;
        if let Some((k, delim)) = l.split_once("<<") {
            let mut v = vec![];
            while i < lines.len() && lines[i] != delim {
                v.push(lines[i]);
                i += 1;
            }
            i += 1;
            put(&mut out, k, v.join("\n"));
        } else if let Some((k, v)) = l.split_once('=') {
            put(&mut out, k, v);
        }
    }
    out
}

/// Merge what a step produced (files + `::commands`) into the job state and the `steps` context.
pub fn absorb(ctx: &JobCtx, st: &mut St, step: &Step, cap: Captured, ok: bool) {
    let n = st.step_files;
    let dir = ctx.sb.tmp_host.join("files").join(n.to_string());
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    let mut outputs = parse_command_file(&read("output"));
    for kv in cap.outputs {
        put(&mut outputs, &kv.key, kv.value);
    }
    for kv in parse_command_file(&read("env")).into_iter().chain(cap.env) {
        ctx.masks.lock().unwrap().retain(|_| true);
        put(&mut st.gh_env, &kv.key, kv.value);
    }
    for l in read("path").lines().map(|l| l.trim().to_string()).chain(cap.paths) {
        if !l.is_empty() && !st.path_add.contains(&l) {
            st.path_add.insert(0, l);
        }
    }
    let summary = read("summary");
    if !summary.trim().is_empty() {
        ctx.emit(&format!("(step summary: {} line(s))", summary.lines().count()));
    }
    if let Some(id) = &step.id {
        let mut o = Map::new();
        for kv in &outputs {
            o.insert(kv.key.clone(), json!(kv.value));
        }
        let outcome = if ok { "success" } else { "failure" };
        let conclusion = if !ok && step.continue_on_error { "success" } else { outcome };
        if let Some(sc) = st.scopes.last_mut() {
            sc.steps.insert(id.clone(), json!({"outputs": Value::Object(o), "outcome": outcome, "conclusion": conclusion}));
        }
    }
}

pub fn record_failure(_ctx: &JobCtx, _st: &mut St, _step: &Step) {}

pub fn start_job(ctx: &JobCtx, _st: &mut St) {
    let _ = std::fs::create_dir_all(ctx.sb.tmp_host.join("tmp"));
    ctx.emit(&format!("Runner: Stackyard simulated GitHub runner ({})", if ctx.image.is_some() { "container" } else { "this machine" }));
    if let Some(i) = &ctx.image {
        ctx.emit(&format!("Image: {i}"));
    }
    ctx.emit(&format!("Event: {}  Ref: {}  SHA: {}", event_name(ctx.run), ctx.run.trig.ref_name, ctx.run.trig.short_sha));
}

pub fn finish_job(ctx: &JobCtx, st: &mut St) {
    // Post steps run in reverse order, even after a failure (cache saves only after success).
    let posts: Vec<PostAction> = st.post.drain(..).rev().collect();
    ctx.cur.set(-1);
    for p in posts {
        match p {
            PostAction::CacheSave { key, paths, hit } => {
                if st.failed || ctx.run.cancel.load(Ordering::SeqCst) {
                    continue;
                }
                if hit {
                    ctx.emit(&format!("Cache hit occurred on the primary key `{key}`, not saving cache."));
                    continue;
                }
                cache_save(ctx, &key, &paths);
            }
            PostAction::NodeAction { dir_in, script, env, name } => {
                ctx.emit(&format!("Post {name}"));
                let mut cap = Captured::default();
                let _ = ctx.exec_captured(&script, &Shell::Sh, &env, &dir_in, Some(Duration::from_secs(300)), true, &mut cap);
            }
        }
    }
    // Job outputs.
    if !ctx.job.outputs.is_empty() {
        let env = resolved_env(ctx, st, &[]);
        let root = ctx_root(ctx, st, &env, if st.failed { "failure" } else { "success" });
        let hf = hash_fn(ctx);
        let cx = GhCtx { root: &root, ok: !st.failed, cancelled: false, hash_files: &hf };
        let mut outs = vec![];
        for kv in &ctx.job.outputs {
            outs.push(Kv::new(&kv.key, gh_interpolate(&kv.value, &cx).unwrap_or_default()));
        }
        ctx.run.jobs[ctx.idx].dynamic.lock().unwrap().outputs = outs;
    }
}

// ============================== caches ==============================

fn cache_dir_host(ctx: &JobCtx) -> PathBuf {
    let d = ctx.run.ci_dir.join("gh-cache").join(slug(&ctx.run.trig.project_name));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn cache_dir_in(ctx: &JobCtx) -> String {
    if ctx.sb.is_docker() {
        format!("/dwci-cache/{}", slug(&ctx.run.trig.project_name))
    } else {
        cache_dir_host(ctx).to_string_lossy().replace('\\', "/")
    }
}

fn cache_hash(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn cache_find(ctx: &JobCtx, key: &str, restore: &[String]) -> Option<(String, bool)> {
    let dir = cache_dir_host(ctx);
    let exact = cache_hash(key);
    if dir.join(format!("{exact}.tgz")).exists() {
        return Some((exact, true));
    }
    for prefix in restore {
        let mut best: Option<(std::time::SystemTime, String)> = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "key").unwrap_or(false) {
                    if let Ok(k) = std::fs::read_to_string(&p) {
                        if k.starts_with(prefix.as_str()) {
                            let m = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                            if best.as_ref().map(|b| m > b.0).unwrap_or(true) && dir.join(format!("{name}.tgz")).exists() {
                                best = Some((m, name));
                            }
                        }
                    }
                }
            }
        }
        if let Some((_, name)) = best {
            return Some((name, false));
        }
    }
    None
}

fn cache_restore(ctx: &JobCtx, hash: &str) -> bool {
    let file = format!("{}/{hash}.tgz", cache_dir_in(ctx));
    let mut cap = Captured::default();
    let r = ctx.exec_captured(&format!("tar -xzPf '{file}'"), &Shell::Sh, &ctx.env, &ctx.sb.ws_in, Some(Duration::from_secs(600)), false, &mut cap);
    matches!(r, Ok(0))
}

fn cache_save(ctx: &JobCtx, key: &str, paths: &[String]) {
    let hash = cache_hash(key);
    let file = format!("{}/{hash}.tgz", cache_dir_in(ctx));
    let mut script = String::from("cd \"$GITHUB_WORKSPACE\"\nlist=\"\"\n");
    for p in paths.iter().filter(|p| !p.starts_with('!')) {
        script.push_str(&format!(
            "p={}\ncase \"$p\" in \"~\"*) p=\"$HOME${{p#\"~\"}}\";; esac\ncase \"$p\" in /*) ;; *) p=\"$PWD/$p\";; esac\nfor m in $p; do [ -e \"$m\" ] && list=\"$list $m\"; done\n",
            exec::single_quote(p)
        ));
    }
    script.push_str(&format!("if [ -n \"$list\" ]; then tar -czPf '{file}' $list && echo \"Cache saved: $(du -sh '{file}' | cut -f1)\"; else echo 'No files matched the cache paths; nothing saved'; fi\n"));
    let mut cap = Captured::default();
    let r = ctx.exec_captured(&script, &Shell::Bash, &ctx.env, &ctx.sb.ws_in, Some(Duration::from_secs(900)), false, &mut cap);
    if matches!(r, Ok(0)) {
        let _ = std::fs::write(cache_dir_host(ctx).join(format!("{hash}.key")), key);
        ctx.emit(&format!("Cache saved with key: {key}"));
    } else {
        ctx.emit("Warning: saving the cache failed");
    }
}

// ============================== uses: ==============================

struct Inputs(Vec<(String, String)>);

impl Inputs {
    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    fn or(&self, k: &str, d: &str) -> String {
        self.get(k).filter(|v| !v.is_empty()).unwrap_or(d).to_string()
    }
}

fn input_env_name(k: &str) -> String {
    format!("INPUT_{}", k.replace(' ', "_").to_uppercase())
}

const SKIP: &[&str] = &[
    "codecov/", "coverallsapp/", "softprops/action-gh-release", "actions/create-release", "actions/upload-release-asset", "peaceiris/", "jamesives/", "slackapi/", "8398a7/", "rtcamp/action-slack-notify",
    "actions/deploy-pages", "actions/configure-pages", "actions/upload-pages-artifact", "github/codeql-action", "actions/stale", "actions/labeler", "actions/attest", "sigstore/", "aws-actions/", "azure/login", "google-github-actions/auth",
    "docker/login-action", "docker/setup-buildx-action", "docker/setup-qemu-action", "docker/metadata-action", "actions/dependency-review-action",
];

pub fn run_uses(ctx: &JobCtx, step: &Step, action: &str, with: &[Kv], st: &mut St) -> StepRes {
    let mut inputs = Inputs(vec![]);
    for kv in with {
        match interp_step(ctx, st, &kv.value) {
            Ok(v) => inputs.0.push((kv.key.clone(), v)),
            Err(e) => {
                ctx.emit(&format!("Error: {e}"));
                return StepRes::Failed(e);
            }
        }
    }
    let name = action.split('@').next().unwrap_or(action).to_lowercase();
    let reff = action.split_once('@').map(|x| x.1).unwrap_or("");
    ctx.emit(&format!("Run {action}"));
    let mut env = step_env(ctx, st, step);
    let mut cap = Captured::default();
    let res = if SKIP.iter().any(|p| name.starts_with(p)) {
        ctx.emit(&format!("`{name}` publishes or talks to an external service; skipped by the simulator."));
        StepRes::Ok
    } else {
        match name.as_str() {
            "actions/checkout" => checkout(ctx, &inputs),
            "actions/setup-node" => setup_node(ctx, st, &inputs, &env, &mut cap),
            "actions/setup-python" | "actions/setup-java" | "actions/setup-go" | "actions/setup-dotnet" | "ruby/setup-ruby" | "actions/setup-ruby" | "dtolnay/rust-toolchain" => check_tool(ctx, &name, &inputs, &env),
            "actions/upload-artifact" => upload_artifact(ctx, &inputs, &mut cap),
            "actions/download-artifact" => download_artifact(ctx, &inputs),
            "actions/cache" | "actions/cache/restore" | "actions/cache/save" => cache_action(ctx, st, &name, &inputs, &mut cap),
            _ => third_party(ctx, step, action, &name, reff, &inputs, &mut env, st, &mut cap),
        }
    };
    let ok = matches!(res, StepRes::Ok | StepRes::Skipped(_));
    absorb(ctx, st, step, cap, ok);
    res
}

fn checkout(ctx: &JobCtx, i: &Inputs) -> StepRes {
    let t = &ctx.run.trig;
    if let Some(repo) = i.get("repository").filter(|r| !r.is_empty() && **r != repository(ctx.run)) {
        let path = i.or("path", repo.rsplit('/').next().unwrap_or("repo"));
        let dest = ctx.sb.ws_host.join(&path);
        ctx.emit(&format!("Checking out external repository {repo} into {path}"));
        let url = format!("https://github.com/{repo}.git");
        let reff = i.get("ref").filter(|r| !r.is_empty());
        let mut args: Vec<&str> = vec!["clone", "--depth", "1"];
        if let Some(r) = reff {
            args.push("--branch");
            args.push(r);
        }
        args.push(&url);
        let d = dest.to_string_lossy().into_owned();
        args.push(&d);
        return match run_cmd("git", &args, None) {
            Ok(o) if o.ok => StepRes::Ok,
            Ok(o) => StepRes::Failed(format!("git clone failed: {}", o.error_text())),
            Err(e) => StepRes::Failed(e),
        };
    }
    ctx.emit(&format!("Syncing repository: {}", repository(ctx.run)));
    ctx.emit(&format!("Checked out {} at {} ({})", t.ref_name, t.short_sha, if t.working { "with your uncommitted changes" } else { "clean clone, like a server runner" }));
    if i.get("path").map(|p| !p.is_empty() && p != ".").unwrap_or(false) {
        ctx.emit("note: `path:` is ignored; the repository stays at the workspace root");
    }
    if i.get("fetch-depth") == Some("1") {
        ctx.emit("note: the simulator always provides the full history (`fetch-depth` is ignored)");
    }
    StepRes::Ok
}

fn setup_node(ctx: &JobCtx, st: &mut St, i: &Inputs, env: &[Kv], cap: &mut Captured) -> StepRes {
    let mut spec = i.or("node-version", "");
    if spec.is_empty() {
        let f = i.or("node-version-file", ".nvmrc");
        spec = std::fs::read_to_string(ctx.sb.ws_host.join(&f)).map(|t| t.trim().trim_start_matches('v').to_string()).unwrap_or_default();
        if spec.is_empty() {
            spec = std::fs::read_to_string(ctx.sb.ws_host.join(".node-version")).map(|t| t.trim().trim_start_matches('v').to_string()).unwrap_or_else(|_| "lts/*".into());
        }
    }
    let want = spec.trim().trim_start_matches('v').trim_start_matches('^').trim_start_matches('~').trim_end_matches(".x").trim_end_matches(".x").to_string();
    let lts = want.starts_with("lts") || want == "latest" || want == "node" || want == "current";
    if !ctx.sb.is_docker() {
        let mut out = String::new();
        let _ = ctx.sb.exec("node --version", &Shell::Sh, env, &ctx.sb.ws_in, &ctx.run.cancel, Some(Duration::from_secs(20)), &mut |l| out.push_str(l));
        let have = out.trim().trim_start_matches('v').to_string();
        ctx.emit(&format!("Using the Node.js on this machine: v{have} (wanted {spec}); the simulator cannot install Node outside a container"));
        return StepRes::Ok;
    }
    let script = format!(
        r#"set -e
want='{want}'
lts={lts}
have=""
if command -v node >/dev/null 2>&1; then have="$(node -p 'process.versions.node')"; fi
if [ -n "$have" ]; then
  case "$have" in "$want"|"$want".*) echo "Node $have already satisfies $want"; exit 0;; esac
  if [ "$lts" = "1" ]; then echo "Using preinstalled Node $have"; exit 0; fi
fi
arch="$(uname -m)"; case "$arch" in x86_64) arch=x64;; aarch64|arm64) arch=arm64;; esac
if [ "$lts" = "1" ]; then
  ver="$(curl -fsSL https://nodejs.org/dist/index.json | grep -m1 '"lts":"' | sed 's/.*"version":"v\([^"]*\)".*/\1/')"
else
  ver="$(curl -fsSL https://nodejs.org/dist/index.json | grep -m1 "\"version\":\"v$want\(\.\|\"\)" | sed 's/.*"version":"v\([^"]*\)".*/\1/')"
fi
[ -n "$ver" ] || {{ echo "Could not resolve Node.js version '$want'"; exit 1; }}
dir="/opt/hostedtoolcache/node/$ver/$arch"
if [ ! -x "$dir/bin/node" ]; then
  echo "Downloading Node.js v$ver"
  mkdir -p "$dir"
  curl -fsSL "https://nodejs.org/dist/v$ver/node-v$ver-linux-$arch.tar.xz" | tar -xJ -C "$dir" --strip-components=1
fi
echo "$dir/bin" >> "$GITHUB_PATH"
echo "Installed Node.js v$ver"
"#,
        lts = if lts { 1 } else { 0 }
    );
    let mut e = env.to_vec();
    put(&mut e, "NODE_VERSION_SPEC", spec);
    let r = ctx.exec_captured(&script, &Shell::Sh, &e, &ctx.sb.ws_in, Some(Duration::from_secs(600)), true, cap);
    let _ = st;
    match r {
        Ok(0) => StepRes::Ok,
        Ok(c) => StepRes::Failed(format!("setup-node failed (exit {c})")),
        Err(e) if e == "cancelled" => StepRes::Cancelled,
        Err(e) => StepRes::Failed(e),
    }
}

fn check_tool(ctx: &JobCtx, name: &str, i: &Inputs, env: &[Kv]) -> StepRes {
    let (cmd, key) = match name {
        "actions/setup-python" => ("python3 --version || python --version", "python-version"),
        "actions/setup-java" => ("java -version 2>&1 | head -n1", "java-version"),
        "actions/setup-go" => ("go version", "go-version"),
        "actions/setup-dotnet" => ("dotnet --version", "dotnet-version"),
        "dtolnay/rust-toolchain" => ("rustc --version", "toolchain"),
        _ => ("ruby --version", "ruby-version"),
    };
    let want = i.or(key, "(default)");
    let mut out = String::new();
    let r = ctx.sb.exec(cmd, &Shell::Sh, env, &ctx.sb.ws_in, &ctx.run.cancel, Some(Duration::from_secs(30)), &mut |l| out.push_str(&format!("{l} ")));
    match r {
        Ok(0) => ctx.emit(&format!("{name}: requested {want}; this environment provides: {}. The simulator does not install other versions, so results may differ from the server.", out.trim())),
        _ => ctx.emit(&format!("{name}: requested {want}, but the tool was not found in this environment. The simulator cannot install it; later steps may fail.")),
    }
    StepRes::Ok
}

fn rel_in_ws(ctx: &JobCtx, p: &str) -> String {
    let n = p.replace('\\', "/");
    let ws = ctx.sb.ws_in.trim_end_matches('/').to_string();
    n.strip_prefix(&format!("{ws}/")).map(String::from).unwrap_or(n)
}

fn upload_artifact(ctx: &JobCtx, i: &Inputs, cap: &mut Captured) -> StepRes {
    let name = i.or("name", "artifact");
    let pats: Vec<String> = i.or("path", "").lines().map(|l| rel_in_ws(ctx, l.trim())).filter(|l| !l.is_empty()).collect();
    let (inc, exc): (Vec<String>, Vec<String>) = pats.iter().cloned().partition(|p| !p.starts_with('!'));
    let exc: Vec<String> = exc.iter().map(|p| p.trim_start_matches('!').to_string()).collect();
    let files = collect_files(&ctx.sb.ws_host, &inc, &exc);
    if files.is_empty() {
        return match i.or("if-no-files-found", "warn").as_str() {
            "error" => StepRes::Failed("No files were found with the provided path".into()),
            "ignore" => StepRes::Ok,
            _ => {
                ctx.emit("Warning: No files were found with the provided path. No artifacts will be uploaded.");
                StepRes::Ok
            }
        };
    }
    let dest = ctx.run.dir.join("gh-artifacts").join(&name);
    if dest.exists() && !i.or("overwrite", "false").eq("true") {
        // v4 artifacts are immutable; mirror that.
        return StepRes::Failed(format!("Failed to CreateArtifact: an artifact named `{name}` already exists in this run"));
    }
    // Like GitHub, files are stored relative to the least common ancestor directory of everything matched.
    let mut root: Option<PathBuf> = None;
    for f in &files {
        let parent = f.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        root = Some(match root {
            None => parent,
            Some(r) => r.components().zip(parent.components()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect(),
        });
    }
    let root = root.unwrap_or_default();
    let mut n = 0;
    let mut stored = vec![];
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f);
        let dst = dest.join(rel);
        if let Some(p) = dst.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if std::fs::copy(ctx.sb.ws_host.join(f), &dst).is_ok() {
            n += 1;
            stored.push(format!("{name}/{}", rel.to_string_lossy().replace('\\', "/")));
        }
    }
    ctx.emit(&format!("Uploaded artifact `{name}`: {n} file(s)"));
    ctx.run.jobs[ctx.idx].dynamic.lock().unwrap().artifacts.extend(stored);
    cap.outputs.push(Kv::new("artifact-id", "1"));
    cap.outputs.push(Kv::new("artifact-url", format!("https://github.com/{}/actions/runs/{}/artifacts/1", repository(ctx.run), ctx.run.number)));
    StepRes::Ok
}

fn download_artifact(ctx: &JobCtx, i: &Inputs) -> StepRes {
    let root = ctx.run.dir.join("gh-artifacts");
    let dest = {
        let p = rel_in_ws(ctx, &i.or("path", "."));
        ctx.sb.ws_host.join(p)
    };
    let names: Vec<String> = match i.get("name").filter(|n| !n.is_empty()) {
        Some(n) => vec![n.to_string()],
        None => std::fs::read_dir(&root).map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default(),
    };
    if names.is_empty() {
        return StepRes::Failed("Unable to find any artifacts for the associated workflow".into());
    }
    let merge = i.or("merge-multiple", "false") == "true" || i.get("name").is_some();
    for n in &names {
        let src = root.join(n);
        if !src.exists() {
            return StepRes::Failed(format!("Unable to download artifact(s): Artifact not found for name: {n}"));
        }
        let to = if merge { dest.clone() } else { dest.join(n) };
        let k = copy_dir(&src, &to);
        ctx.emit(&format!("Downloaded artifact `{n}`: {k} file(s)"));
    }
    StepRes::Ok
}

fn copy_dir(from: &Path, to: &Path) -> usize {
    let files = collect_files(from, &[".".to_string()], &[]);
    copy_files(from, to, &files)
}

fn cache_action(ctx: &JobCtx, st: &mut St, name: &str, i: &Inputs, cap: &mut Captured) -> StepRes {
    let key = i.or("key", "");
    if key.is_empty() {
        return StepRes::Failed("`key` is required".into());
    }
    let paths: Vec<String> = i.or("path", "").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let restore_keys: Vec<String> = i.or("restore-keys", "").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let mode_save = name.ends_with("/save");
    let mode_restore = name.ends_with("/restore");
    if mode_save {
        cache_save(ctx, &key, &paths);
        return StepRes::Ok;
    }
    let found = cache_find(ctx, &key, &restore_keys);
    let mut hit = false;
    match &found {
        Some((hash, exact)) => {
            let ok = cache_restore(ctx, hash);
            if ok {
                hit = *exact;
                ctx.emit(&format!("Cache restored from key: {}", if *exact { key.clone() } else { "(restore-keys prefix)".into() }));
            } else {
                ctx.emit("Warning: could not restore the cache archive");
            }
            cap.outputs.push(Kv::new("cache-hit", hit.to_string()));
            cap.outputs.push(Kv::new("cache-matched-key", if *exact { key.clone() } else { String::new() }));
        }
        None => {
            ctx.emit(&format!("Cache not found for input keys: {key}"));
            cap.outputs.push(Kv::new("cache-hit", "false"));
        }
    }
    if !mode_restore {
        st.post.push(PostAction::CacheSave { key, paths, hit });
    }
    StepRes::Ok
}

// ============================== third-party actions ==============================

fn fetch_action(ctx: &JobCtx, owner_repo: &str, reff: &str) -> Result<PathBuf, String> {
    let dirname = format!("{}__{}", owner_repo.replace('/', "__"), slug(reff));
    let dir = ctx.run.ci_dir.join("actions").join(&dirname);
    if dir.join(".git").exists() || dir.join("action.yml").exists() || dir.join("action.yaml").exists() {
        return Ok(dir);
    }
    ctx.emit(&format!("Download action repository '{owner_repo}@{reff}'"));
    let url = format!("https://github.com/{owner_repo}.git");
    let d = dir.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.parent().unwrap()).map_err(|e| e.to_string())?;
    let direct = run_cmd("git", &["clone", "--quiet", "--depth", "1", "--branch", reff, &url, &d], None)?;
    if direct.ok {
        return Ok(dir);
    }
    // A commit SHA cannot be cloned with --branch.
    let _ = std::fs::remove_dir_all(&dir);
    let c = run_cmd("git", &["clone", "--quiet", &url, &d], None)?;
    if !c.ok {
        return Err(format!("could not download {owner_repo}@{reff}: {}", c.error_text()));
    }
    let k = run_cmd("git", &["-C", &d, "checkout", "--quiet", reff], None)?;
    if !k.ok {
        return Err(format!("could not check out {reff}: {}", k.error_text()));
    }
    Ok(dir)
}

fn action_dir_in(ctx: &JobCtx, host: &Path) -> String {
    if ctx.sb.is_docker() {
        let rel = host.strip_prefix(ctx.run.ci_dir.join("actions")).ok().map(|p| p.to_string_lossy().replace('\\', "/"));
        match rel {
            Some(r) => format!("/dwci-actions/{r}"),
            None => {
                // Local action inside the workspace.
                host.strip_prefix(&ctx.sb.ws_host).ok().map(|p| format!("{}/{}", ctx.sb.ws_in, p.to_string_lossy().replace('\\', "/"))).unwrap_or_else(|| host.to_string_lossy().replace('\\', "/"))
            }
        }
    } else {
        host.to_string_lossy().replace('\\', "/")
    }
}

#[allow(clippy::too_many_arguments)]
fn third_party(ctx: &JobCtx, step: &Step, action: &str, name: &str, reff: &str, inputs: &Inputs, env: &mut Vec<Kv>, st: &mut St, cap: &mut Captured) -> StepRes {
    let _ = name;
    let spec = action.split('@').next().unwrap_or(action);
    // Resolve to a directory containing action.yml
    let host_dir: PathBuf = if let Some(local) = spec.strip_prefix("./") {
        ctx.sb.ws_host.join(local)
    } else if let Some(img) = spec.strip_prefix("docker://") {
        return run_docker_image(ctx, img, &[], inputs, env, cap);
    } else {
        let parts: Vec<&str> = spec.splitn(3, '/').collect();
        if parts.len() < 2 || reff.is_empty() {
            return StepRes::Failed(format!("invalid action reference `{action}` (expected owner/repo@ref)"));
        }
        let owner_repo = format!("{}/{}", parts[0], parts[1]);
        match fetch_action(ctx, &owner_repo, reff) {
            Ok(d) => {
                if parts.len() == 3 {
                    d.join(parts[2])
                } else {
                    d
                }
            }
            Err(e) => {
                ctx.emit(&e);
                return StepRes::Failed(e);
            }
        }
    };
    let manifest = ["action.yml", "action.yaml"].iter().map(|f| host_dir.join(f)).find(|p| p.exists());
    let Some(manifest) = manifest else {
        let m = format!("`{action}` has no action.yml");
        ctx.emit(&m);
        return StepRes::Failed(m);
    };
    let doc: Y = match std::fs::read_to_string(&manifest).ok().and_then(|t| serde_yaml::from_str(&t).ok()) {
        Some(d) => d,
        None => return StepRes::Failed("could not read action.yml".into()),
    };
    // Effective inputs: defaults from the manifest, overridden by `with:`
    let mut effective: Vec<(String, String)> = vec![];
    if let Some(Y::Mapping(m)) = doc.get("inputs") {
        for (k, v) in m {
            if let Some(k) = s(k) {
                effective.push((k, v.get("default").and_then(s).unwrap_or_default()));
            }
        }
    }
    for (k, v) in &inputs.0 {
        if let Some(e) = effective.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            e.1 = v.clone();
        } else {
            effective.push((k.clone(), v.clone()));
        }
    }
    let dir_in = action_dir_in(ctx, &host_dir);
    for (k, v) in &effective {
        put(env, &input_env_name(k), v.clone());
        if st.scopes.len() >= 1 {
            // secrets used as inputs are masked in logs
        }
        let _ = v;
    }
    put(env, "GITHUB_ACTION_PATH", dir_in.clone());
    put(env, "GITHUB_ACTION_REPOSITORY", spec.to_string());
    put(env, "GITHUB_ACTION_REF", reff.to_string());
    let runs = doc.get("runs");
    let using = runs.and_then(|r| r.get("using")).and_then(s).unwrap_or_default();
    if using.starts_with("node") {
        let main = runs.and_then(|r| r.get("main")).and_then(s).unwrap_or_default();
        if main.is_empty() {
            return StepRes::Failed("action.yml has no `runs.main`".into());
        }
        if let Some(pre) = runs.and_then(|r| r.get("pre")).and_then(s) {
            let mut c2 = Captured::default();
            let _ = ctx.exec_captured(&format!("node \"{dir_in}/{pre}\""), &Shell::Sh, env, &ctx.sb.ws_in, Some(Duration::from_secs(600)), true, &mut c2);
        }
        let r = ctx.exec_captured(&format!("node \"{dir_in}/{main}\""), &Shell::Sh, env, &ctx.sb.ws_in, ctx.remaining_for(step), true, cap);
        if let Some(post) = runs.and_then(|r| r.get("post")).and_then(s) {
            st.post.push(PostAction::NodeAction { dir_in: ctx.sb.ws_in.clone(), script: format!("node \"{dir_in}/{post}\""), env: env.clone(), name: spec.to_string() });
        }
        return match r {
            Ok(0) => StepRes::Ok,
            Ok(c) => StepRes::Failed(format!("action exited with code {c}")),
            Err(e) if e == "cancelled" => StepRes::Cancelled,
            Err(e) => StepRes::Failed(e),
        };
    }
    if using == "composite" {
        return run_composite(ctx, step, &doc, &effective, &dir_in, st, cap);
    }
    if using == "docker" {
        let image = runs.and_then(|r| r.get("image")).and_then(s).unwrap_or_default();
        let args: Vec<String> = list_of_str(runs.and_then(|r| r.get("args")));
        if let Some(img) = image.strip_prefix("docker://") {
            return run_docker_image(ctx, img, &args, inputs, env, cap);
        }
        // Dockerfile action: build it first.
        let tag = format!("dwci-action-{}", slug(spec));
        ctx.emit(&format!("Building action image from {}", host_dir.display()));
        let mut c = command("docker");
        c.args(["build", "-t", &tag, "-f", &host_dir.join(&image).to_string_lossy(), &host_dir.to_string_lossy()]);
        let r = exec::run_streaming(c, &ctx.run.cancel, Some(Duration::from_secs(1800)), &mut |l| ctx.emit(l));
        if !matches!(r, Ok(0)) {
            return StepRes::Failed("could not build the action's Dockerfile".into());
        }
        return run_docker_image(ctx, &tag, &args, inputs, env, cap);
    }
    StepRes::Failed(format!("unsupported action type `{using}`"))
}

impl<'a> JobCtx<'a> {
    pub fn remaining_for(&self, step: &Step) -> Option<Duration> {
        let own = step.timeout_secs.map(Duration::from_secs);
        let rem = self.deadline.map(|d| d.saturating_duration_since(std::time::Instant::now()));
        match (own, rem) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

fn run_docker_image(ctx: &JobCtx, image: &str, args: &[String], inputs: &Inputs, env: &[Kv], cap: &mut Captured) -> StepRes {
    if !ctx.run.docker {
        return StepRes::Failed("Docker actions need Docker".into());
    }
    let mut emit = |l: &str| ctx.emit(l);
    if let Err(e) = exec::ensure_image(image, &ctx.run.cancel, &mut emit) {
        return StepRes::Failed(e);
    }
    let mut c = command("docker");
    c.args(["run", "--rm", "-v"]).arg(format!("{}:/github/workspace", ctx.sb.ws_host.to_string_lossy())).args(["-w", "/github/workspace"]);
    c.arg("-v").arg(format!("{}:/github/file_commands", ctx.sb.tmp_host.join("files").join("1").to_string_lossy()));
    for kv in env {
        if kv.key.starts_with("GITHUB_") || kv.key.starts_with("RUNNER_") || kv.key.starts_with("INPUT_") || kv.key == "CI" {
            c.arg("-e").arg(format!("{}={}", kv.key, kv.value));
        }
    }
    for (k, v) in &inputs.0 {
        c.arg("-e").arg(format!("{}={v}", input_env_name(k)));
    }
    c.arg(image);
    c.args(args);
    let mut wf = |l: &str| {
        ctx.emit(l);
    };
    let _ = cap;
    match exec::run_streaming(c, &ctx.run.cancel, ctx.remaining_for(&Step::new("docker", StepKind::Sleep { seconds: 0 })), &mut wf) {
        Ok(0) => StepRes::Ok,
        Ok(n) => StepRes::Failed(format!("container action exited with code {n}")),
        Err(e) if e == "cancelled" => StepRes::Cancelled,
        Err(e) => StepRes::Failed(e),
    }
}

fn run_composite(ctx: &JobCtx, step: &Step, doc: &Y, inputs: &[(String, String)], dir_in: &str, st: &mut St, cap: &mut Captured) -> StepRes {
    let Some(Y::Sequence(steps)) = doc.get("runs").and_then(|r| r.get("steps")) else {
        return StepRes::Failed("composite action has no steps".into());
    };
    let mut inp = Map::new();
    for (k, v) in inputs {
        inp.insert(k.clone(), json!(v));
    }
    st.scopes.push(GhScope { inputs: Value::Object(inp), steps: Map::new(), action_path: dir_in.to_string() });
    let mut warnings = vec![];
    let sub: Vec<Step> = steps.iter().enumerate().map(|(i, y)| super::github::parse_step(y, i, &mut warnings, "composite")).collect();
    let before_failed = st.failed;
    ctx.run_steps(&sub, st, Some(ctx.cur.get().max(0) as usize));
    // Outputs of the composite action
    let mut outs = vec![];
    if let Some(Y::Mapping(m)) = doc.get("outputs") {
        let env = resolved_env(ctx, st, &[]);
        let root = ctx_root(ctx, st, &env, "success");
        let hf = hash_fn(ctx);
        let cx = GhCtx { root: &root, ok: true, cancelled: false, hash_files: &hf };
        for (k, v) in m {
            if let (Some(k), Some(val)) = (s(k), v.get("value").and_then(s)) {
                outs.push(Kv::new(k, gh_interpolate(&val, &cx).unwrap_or_default()));
            }
        }
    }
    st.scopes.pop();
    cap.outputs.extend(outs);
    let _ = step;
    if st.failed && !before_failed {
        StepRes::Failed("a step in the composite action failed".into())
    } else {
        StepRes::Ok
    }
}
