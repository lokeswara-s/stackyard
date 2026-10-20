//! Pipeline runs: planning, scheduling, state and logs. Job execution lives in `runner.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::exec;
use super::expr;
use super::ir::*;
use super::{discover, load};
use crate::model::EnvVar;
use crate::util::{now_ms, run, sanitize_name};

const MAX_RUNS: usize = 12;
pub const LOG_CAP: usize = 80_000;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    pub provider: String,
    pub file: String,
    pub repo_path: String,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub git_ref: String,
    /// committed (a clean checkout of the commit, like a server) | working (include uncommitted changes)
    #[serde(default)]
    pub source: String,
    /// auto | docker | host
    #[serde(default)]
    pub runner: String,
    /// Run exactly these jobs, whatever their rules / conditions say (empty = the whole pipeline).
    #[serde(default)]
    pub only_jobs: Vec<String>,
    /// With `only_jobs`: also run every job they depend on (so artifacts and outputs exist).
    #[serde(default)]
    pub with_needs: bool,
    /// With a single job in `only_jobs`: only these steps of it (indexes; empty = all steps).
    #[serde(default)]
    pub only_steps: Vec<usize>,
    #[serde(default)]
    pub variables: Vec<Kv>,
    #[serde(default)]
    pub ubuntu_image: String,
    #[serde(default)]
    pub max_parallel: usize,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub branch: String,
    pub tag: String,
    pub ref_name: String,
    pub sha: String,
    pub short_sha: String,
    pub before_sha: String,
    pub message: String,
    pub title: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: String,
    pub default_branch: String,
    pub origin_url: String,
    pub project_name: String,
    pub namespace: String,
    pub changed: Vec<String>,
    pub event: String,
    pub working: bool,
}

#[derive(Clone, Debug, Default)]
pub struct JobDyn {
    /// pending | running | passed | failed | warning | skipped | canceled | manual
    pub status: String,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    pub attempt: u32,
    pub note: Option<String>,
    pub steps: Vec<StepDyn>,
    pub artifacts: Vec<String>,
    pub outputs: Vec<Kv>,
    pub dotenv: Vec<Kv>,
    pub forced: bool,
    /// The user picked this job: it runs regardless of rules / `if` / failed dependencies (after they finish).
    pub selected: bool,
    pub only_steps: Vec<usize>,
    pub runner: String,
    pub coverage: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StepDyn {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub step: i32,
    pub ts: u64,
    pub text: String,
}

pub struct JobState {
    pub job: Job,
    pub dynamic: Mutex<JobDyn>,
    pub log: Mutex<Vec<LogLine>>,
}

pub struct Run {
    pub id: String,
    pub number: u64,
    pub opts: RunOptions,
    pub pipeline: Pipeline,
    pub trig: Trigger,
    pub ci_vars: Vec<EnvVar>,
    pub dir: PathBuf,
    pub ci_dir: PathBuf,
    pub started_ms: u64,
    pub finished_ms: AtomicU64,
    pub status: Mutex<String>,
    pub cancel: AtomicBool,
    pub jobs: Vec<JobState>,
    pub docker: bool,
    pub notes: Mutex<Vec<String>>,
    pub shared_ws: Mutex<Option<PathBuf>>,
    pub driving: AtomicBool,
    /// Jenkins result so far (SUCCESS | UNSTABLE | FAILURE).
    pub build_result: Mutex<String>,
}

impl Run {
    pub fn log(&self, idx: usize, step: i32, text: &str) {
        let js = &self.jobs[idx];
        let mut l = js.log.lock().unwrap();
        if l.len() == LOG_CAP {
            l.push(LogLine { step, ts: now_ms(), text: "... log truncated (too long) ...".into() });
        }
        if l.len() > LOG_CAP {
            return;
        }
        l.push(LogLine { step, ts: now_ms(), text: text.to_string() });
    }

    pub fn find_jobs(&self, reference: &str) -> Vec<usize> {
        let jobs: Vec<&Job> = self.jobs.iter().map(|j| &j.job).collect();
        find_jobs_in(&jobs, reference)
    }

    pub fn stage_index(&self, stage: &str) -> usize {
        self.pipeline.stages.iter().position(|s| s == stage).unwrap_or(0)
    }

    /// Jobs this job waits for.
    pub fn deps_of(&self, idx: usize) -> Vec<usize> {
        let jobs: Vec<&Job> = self.jobs.iter().map(|j| &j.job).collect();
        deps_in(&self.pipeline, &jobs, idx)
    }

    pub fn status_of(&self, idx: usize) -> String {
        self.jobs[idx].dynamic.lock().unwrap().status.clone()
    }

    pub fn mask_values(&self) -> Vec<String> {
        self.ci_vars.iter().filter(|v| v.secret && v.value.len() >= 3).map(|v| v.value.clone()).collect()
    }

    pub fn ci_var(&self, key: &str) -> Option<String> {
        self.ci_vars.iter().find(|v| v.key == key).map(|v| v.value.clone())
    }
}

/// Jobs matching a `needs:` reference (a plain id, or every matrix variant of it).
pub fn find_jobs_in(jobs: &[&Job], reference: &str) -> Vec<usize> {
    jobs.iter()
        .enumerate()
        .filter(|(_, j)| {
            let id = &j.id;
            id == reference || id.starts_with(&format!("{reference} (")) || id.starts_with(&format!("{reference}: [")) || (id.starts_with(&format!("{reference} ")) && id.rsplit(' ').next().map(|x| x.contains('/')).unwrap_or(false))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Jobs `idx` waits for: the whole previous stages (GitLab, Jenkins) or its `needs` (GitHub / `needs:`).
pub fn deps_in(p: &Pipeline, jobs: &[&Job], idx: usize) -> Vec<usize> {
    let stage_index = |s: &str| p.stages.iter().position(|x| x == s).unwrap_or(0);
    let job = jobs[idx];
    if p.gated_by_stage && !job.has_needs {
        let si = stage_index(&job.stage);
        return jobs.iter().enumerate().filter(|(i, j)| *i != idx && stage_index(&j.stage) < si).map(|(i, _)| i).collect();
    }
    let mut out = vec![];
    for n in &job.needs {
        for i in find_jobs_in(jobs, &n.job) {
            if i != idx && !out.contains(&i) {
                out.push(i);
            }
        }
    }
    out
}

// ============================== views ==============================

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub name: String,
    pub stage: String,
    pub status: String,
    pub needs: Vec<String>,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    pub attempt: u32,
    pub note: Option<String>,
    pub allow_failure: bool,
    pub steps: Vec<StepView>,
    pub artifacts: Vec<String>,
    pub runner: String,
    pub log_lines: usize,
    pub coverage: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: String,
    pub number: u64,
    pub provider: String,
    pub file: String,
    pub name: String,
    pub repo_path: String,
    pub status: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub trigger: Trigger,
    pub source: String,
    pub runner: String,
    pub stages: Vec<String>,
    pub jobs: Vec<JobView>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    pub dir: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    pub next: usize,
}

pub struct Ci {
    pub dir: PathBuf,
    runs: Mutex<Vec<Arc<Run>>>,
    counter: AtomicU64,
}

// ============================== git ==============================

fn git(repo: &str, args: &[&str]) -> Option<String> {
    let mut a = vec!["-C", repo];
    a.extend(args);
    run("git", &a, None).ok().filter(|o| o.ok).map(|o| o.stdout.trim().to_string())
}

pub fn git_refs(repo: &str) -> (Vec<String>, Vec<String>, String) {
    let branches = git(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
    let tags = git(repo, &["for-each-ref", "--sort=-creatordate", "--format=%(refname:short)", "refs/tags"]).map(|s| s.lines().take(50).map(String::from).collect()).unwrap_or_default();
    let current = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD").unwrap_or_default();
    (branches, tags, current)
}

pub fn gather_trigger(opts: &RunOptions) -> Result<Trigger, String> {
    let repo = opts.repo_path.as_str();
    if git(repo, &["rev-parse", "--git-dir"]).is_none() {
        return Err(format!("`{repo}` is not a git repository. Pipelines run on a clean checkout, so the project needs to be one."));
    }
    let (branches, tags, current) = git_refs(repo);
    let wanted = opts.git_ref.trim().to_string();
    let (reference, is_tag) = if wanted.is_empty() {
        (if current.is_empty() { "HEAD".to_string() } else { current.clone() }, false)
    } else if tags.contains(&wanted) && !branches.contains(&wanted) {
        (wanted.clone(), true)
    } else {
        (wanted.clone(), false)
    };
    let sha = git(repo, &["rev-parse", "--verify", &format!("{reference}^{{commit}}")]).ok_or_else(|| format!("`{reference}` does not exist in this repository (or there are no commits yet)"))?;
    let sha_for_fmt = sha.clone();
    let fmt = move |f: &str| git(repo, &["log", "-1", &format!("--format={f}"), &sha_for_fmt]).unwrap_or_default();
    let origin = git(repo, &["remote", "get-url", "origin"]).unwrap_or_default();
    let project_name = Path::new(repo).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    // namespace/name from an origin like git@host:ns/name.git or https://host/ns/name
    let (namespace, name_from_origin) = {
        let o = origin.trim_end_matches(".git").replace(':', "/");
        let parts: Vec<&str> = o.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 {
            (parts[parts.len() - 2].to_string(), parts[parts.len() - 1].to_string())
        } else {
            ("local".to_string(), project_name.clone())
        }
    };
    let default_branch = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .map(|s| s.trim_start_matches("origin/").to_string())
        .or_else(|| ["main", "master"].iter().find(|b| branches.iter().any(|x| x == *b)).map(|s| s.to_string()))
        .unwrap_or_else(|| if current.is_empty() { "main".into() } else { current.clone() });
    let before = git(repo, &["rev-parse", &format!("{sha}~1")]).unwrap_or_else(|| "0000000000000000000000000000000000000000".into());
    let changed = git(repo, &["diff-tree", "--no-commit-id", "--name-only", "-r", "--root", "-m", "--first-parent", &sha]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
    let working = opts.source == "working" && wanted.is_empty();
    let (branch, tag) = if is_tag { (String::new(), reference.clone()) } else if reference == "HEAD" { (String::new(), String::new()) } else { (reference.clone(), String::new()) };
    Ok(Trigger {
        ref_name: if is_tag { tag.clone() } else if branch.is_empty() { sha[..8.min(sha.len())].to_string() } else { branch.clone() },
        branch,
        tag,
        short_sha: sha[..8.min(sha.len())].to_string(),
        sha,
        before_sha: before,
        message: fmt("%B").trim().to_string(),
        title: fmt("%s"),
        author_name: fmt("%an"),
        author_email: fmt("%ae"),
        timestamp: fmt("%cI"),
        default_branch,
        origin_url: origin,
        project_name: name_from_origin,
        namespace,
        changed,
        event: if opts.event.is_empty() { "push".into() } else { opts.event.clone() },
        working,
    })
}

// ============================== variables ==============================

/// Expand `$VAR` / `${VAR}` references (GitLab style). `$$` is a literal `$`. Unknown names are kept.
pub fn expand_vars(s: &str, vars: &[Kv]) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '$' {
            out.push(c[i]);
            i += 1;
            continue;
        }
        match c.get(i + 1) {
            Some('$') => {
                out.push('$');
                i += 2;
            }
            Some('{') => {
                if let Some(end) = c[i + 2..].iter().position(|x| *x == '}') {
                    let name: String = c[i + 2..i + 2 + end].iter().collect();
                    match get(vars, &name) {
                        Some(v) => out.push_str(v),
                        None => out.push_str(&c[i..i + 3 + end].iter().collect::<String>()),
                    }
                    i += end + 3;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            Some(ch) if ch.is_alphabetic() || *ch == '_' => {
                let mut j = i + 1;
                while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                    j += 1;
                }
                let name: String = c[i + 1..j].iter().collect();
                match get(vars, &name) {
                    Some(v) => out.push_str(v),
                    None => out.push_str(&c[i..j].iter().collect::<String>()),
                }
                i = j;
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

pub fn slug(s: &str) -> String {
    let mut out: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    out = out.trim_matches('-').to_string();
    out.chars().take(63).collect()
}

/// GitLab predefined variables for the pipeline (job-specific ones are added by the runner).
pub fn gitlab_predefined(run_id: u64, trig: &Trigger, opts: &RunOptions) -> Vec<Kv> {
    let mut v = vec![];
    let mut p = |k: &str, val: &str| put(&mut v, k, val);
    p("CI", "true");
    p("GITLAB_CI", "true");
    p("CI_SERVER", "yes");
    p("CI_SERVER_NAME", "GitLab");
    p("CI_SERVER_HOST", "gitlab.local");
    p("CI_SERVER_URL", "https://gitlab.local");
    p("CI_API_V4_URL", "https://gitlab.local/api/v4");
    p("CI_COMMIT_SHA", &trig.sha);
    p("CI_COMMIT_SHORT_SHA", &trig.short_sha);
    p("CI_COMMIT_BEFORE_SHA", &trig.before_sha);
    p("CI_COMMIT_REF_NAME", &trig.ref_name);
    p("CI_COMMIT_REF_SLUG", &slug(&trig.ref_name));
    if !trig.branch.is_empty() {
        p("CI_COMMIT_BRANCH", &trig.branch);
    }
    if !trig.tag.is_empty() {
        p("CI_COMMIT_TAG", &trig.tag);
    }
    p("CI_COMMIT_MESSAGE", &trig.message);
    p("CI_COMMIT_TITLE", &trig.title);
    p("CI_COMMIT_AUTHOR", &format!("{} <{}>", trig.author_name, trig.author_email));
    p("CI_COMMIT_TIMESTAMP", &trig.timestamp);
    p("CI_DEFAULT_BRANCH", &trig.default_branch);
    p("CI_PROJECT_NAME", &trig.project_name);
    p("CI_PROJECT_TITLE", &trig.project_name);
    p("CI_PROJECT_NAMESPACE", &trig.namespace);
    p("CI_PROJECT_PATH", &format!("{}/{}", trig.namespace, trig.project_name));
    p("CI_PROJECT_PATH_SLUG", &slug(&format!("{}/{}", trig.namespace, trig.project_name)));
    p("CI_PROJECT_ID", "1");
    p("CI_PROJECT_URL", &format!("https://gitlab.local/{}/{}", trig.namespace, trig.project_name));
    p("CI_REPOSITORY_URL", &trig.origin_url);
    p("CI_PIPELINE_ID", &run_id.to_string());
    p("CI_PIPELINE_IID", &run_id.to_string());
    p("CI_PIPELINE_SOURCE", match trig.event.as_str() {
        "merge_request" | "merge_request_event" => "merge_request_event",
        "schedule" => "schedule",
        "web" | "manual" => "web",
        "api" => "api",
        _ => "push",
    });
    p("CI_PIPELINE_URL", &format!("https://gitlab.local/{}/{}/-/pipelines/{run_id}", trig.namespace, trig.project_name));
    p("CI_RUNNER_ID", "1");
    p("CI_RUNNER_DESCRIPTION", "Stackyard local runner");
    p("CI_RUNNER_TAGS", "");
    p("CI_CONCURRENT_ID", "0");
    p("GITLAB_USER_LOGIN", &trig.author_name);
    p("GITLAB_USER_NAME", &trig.author_name);
    p("GITLAB_USER_EMAIL", &trig.author_email);
    p("GITLAB_USER_ID", "1");
    if trig.event == "merge_request" || trig.event == "merge_request_event" {
        p("CI_MERGE_REQUEST_IID", "1");
        p("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", &trig.branch);
        p("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", &trig.default_branch);
        p("CI_MERGE_REQUEST_TITLE", &trig.title);
    }
    let _ = opts;
    v
}

// ============================== rules (GitLab) ==============================

pub struct RuleOutcome {
    pub include: bool,
    pub when: String,
    pub allow_failure: bool,
    pub vars: Vec<Kv>,
}

pub fn eval_rules(job: &Job, vars: &[Kv], trig: &Trigger, repo: &str) -> Result<RuleOutcome, String> {
    let default = RuleOutcome { include: true, when: job.when.clone(), allow_failure: job.allow_failure, vars: vec![] };
    if job.rules.is_empty() {
        return Ok(if job.when == "never" { RuleOutcome { include: false, ..default } } else { default });
    }
    let lookup = |n: &str| get(vars, n).map(String::from);
    for r in &job.rules {
        let mut ok = true;
        if let Some(c) = &r.cond {
            ok = expr::gl_eval(c, &lookup).map_err(|e| format!("rules:if `{c}`: {e}"))?;
        }
        if ok && !r.changes.is_empty() {
            ok = r.changes.iter().any(|g| {
                let pat = glob::Pattern::new(g).ok();
                trig.changed.iter().any(|f| pat.as_ref().map(|p| p.matches_with(f, glob::MatchOptions { require_literal_separator: false, ..Default::default() })).unwrap_or(false) || f == g || f.starts_with(&format!("{}/", g.trim_end_matches("/**"))))
            });
        }
        if ok && !r.exists.is_empty() {
            ok = r.exists.iter().any(|g| glob::glob(&format!("{}/{}", repo.replace('\\', "/"), g)).map(|mut it| it.next().is_some()).unwrap_or(false));
        }
        if ok {
            let when = r.when.clone().unwrap_or_else(|| job.when.clone());
            if when == "never" {
                return Ok(RuleOutcome { include: false, when, allow_failure: false, vars: vec![] });
            }
            let allow = r.allow_failure.unwrap_or(if when == "manual" { false } else { job.allow_failure });
            return Ok(RuleOutcome { include: true, when, allow_failure: allow, vars: r.variables.clone() });
        }
    }
    Ok(RuleOutcome { include: false, when: "never".into(), allow_failure: false, vars: vec![] })
}

// ============================== manager ==============================

impl Ci {
    pub fn new(dir: PathBuf) -> Ci {
        let _ = std::fs::create_dir_all(&dir);
        // Old runs from a previous session are not resumable: clear their working copies.
        // Moved aside first (instant) and deleted in the background: it can hold gigabytes, and startup must not wait for that.
        let runs = dir.join("runs");
        let mut stale: Vec<PathBuf> = vec![];
        if runs.exists() {
            let aside = dir.join(format!("runs-old-{}", now_ms()));
            stale.push(if std::fs::rename(&runs, &aside).is_ok() { aside } else { runs });
        }
        // Earlier sessions that were killed mid-delete.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("runs-old-") && !stale.contains(&e.path()) {
                    stale.push(e.path());
                }
            }
        }
        if !stale.is_empty() {
            std::thread::spawn(move || {
                for p in stale {
                    let _ = std::fs::remove_dir_all(p);
                }
            });
        }
        Ci { dir, runs: Mutex::new(vec![]), counter: AtomicU64::new(0) }
    }

    pub fn pipelines(&self, repo: &str) -> Vec<PipelineFile> {
        discover(Path::new(repo))
    }

    pub fn load(&self, repo: &str, provider: &str, file: &str) -> Result<Pipeline, String> {
        load(Path::new(repo), provider, file)
    }

    pub fn start(self: &Arc<Self>, opts: RunOptions, ci_vars: Vec<EnvVar>) -> Result<String, String> {
        if let Some(free) = crate::storage::free_bytes(&self.dir) {
            if free < crate::storage::MIN_FREE_BYTES {
                return Err(format!(
                    "Only {:.1} GB of disk space is free, and pipelines need room for checkouts and caches. Free some space (Settings → Storage → Clean up) and try again.",
                    free as f64 / 1_073_741_824.0
                ));
            }
        }
        let pipeline = load(Path::new(&opts.repo_path), &opts.provider, &opts.file)?;
        let trig = gather_trigger(&opts)?;
        let number = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("run-{}-{}", number, now_ms() % 100000);
        let docker = match opts.runner.as_str() {
            "host" => false,
            "docker" => {
                if !exec::docker_ready() {
                    return Err("Docker is not running. Start Docker Desktop, or choose \"This machine\" as the runner.".into());
                }
                true
            }
            _ => exec::docker_ready(),
        };

        // Variables visible to `rules:` (GitLab): predefined, global, project variables, run overrides.
        let mut rule_vars = gitlab_predefined(number, &trig, &opts);
        for kv in &pipeline.env {
            put(&mut rule_vars, &kv.key, kv.value.clone());
        }
        for v in &ci_vars {
            put(&mut rule_vars, &v.key, v.value.clone());
        }
        for kv in &opts.variables {
            put(&mut rule_vars, &kv.key, kv.value.clone());
        }

        // Jobs the user picked (plus, optionally, everything they depend on).
        let selection: Option<Vec<bool>> = if opts.only_jobs.is_empty() {
            None
        } else {
            let refs: Vec<&Job> = pipeline.jobs.iter().collect();
            let mut sel: Vec<bool> = pipeline.jobs.iter().map(|j| opts.only_jobs.contains(&j.id)).collect();
            if opts.with_needs {
                let mut stack: Vec<usize> = (0..sel.len()).filter(|i| sel[*i]).collect();
                while let Some(i) = stack.pop() {
                    for d in deps_in(&pipeline, &refs, i) {
                        if !sel[d] {
                            sel[d] = true;
                            stack.push(d);
                        }
                    }
                }
            }
            Some(sel)
        };

        let mut notes = vec![];
        let mut jobs = vec![];
        for (i, j) in pipeline.jobs.iter().enumerate() {
            let mut job = j.clone();
            let mut d = JobDyn { status: "pending".into(), ..Default::default() };
            let picked = selection.as_ref().map(|s| s[i]);
            if picked == Some(false) {
                d.status = "skipped".into();
                d.note = Some("not selected for this run".into());
            } else {
                if picked == Some(true) {
                    // Picked by the user: run it whatever the pipeline's own conditions say.
                    d.selected = true;
                    job.cond = None;
                    if opts.only_jobs.len() == 1 && opts.only_jobs[0] == job.id {
                        d.only_steps = opts.only_steps.clone();
                    }
                }
                if pipeline.provider == "gitlab" {
                    match eval_rules(&job, &rule_vars, &trig, &opts.repo_path) {
                        Ok(o) => {
                            if o.include {
                                job.when = o.when.clone();
                                job.allow_failure = o.allow_failure;
                                for kv in o.vars {
                                    put(&mut job.env, &kv.key, kv.value);
                                }
                            }
                            if picked == Some(true) {
                                if !o.include {
                                    d.note = Some("rules would exclude this job on this ref; running it because you picked it".into());
                                }
                                if job.when == "manual" || job.when == "never" {
                                    job.when = "on_success".into();
                                }
                            } else if !o.include {
                                d.status = "skipped".into();
                                d.note = Some("excluded by rules / only / except for this ref (pick the job to run it anyway)".into());
                            } else if o.when == "manual" {
                                d.status = "manual".into();
                                d.note = Some("manual job: press play to run it".into());
                            }
                        }
                        Err(e) => {
                            notes.push(e.clone());
                            if picked != Some(true) {
                                d.status = "skipped".into();
                                d.note = Some(e);
                            }
                        }
                    }
                    if job.trigger {
                        d.status = "skipped".into();
                        d.note = Some("trigger jobs start downstream pipelines, which the simulator does not run".into());
                    }
                } else if picked == Some(true) && job.when != "always" {
                    job.when = "on_success".into();
                }
            }
            jobs.push(JobState { job, dynamic: Mutex::new(d), log: Mutex::new(vec![]) });
        }
        let dir = self.dir.join("runs").join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let run = Arc::new(Run {
            id: id.clone(),
            number,
            opts: RunOptions { max_parallel: if opts.max_parallel == 0 { 4 } else { opts.max_parallel }, ..opts },
            pipeline,
            trig,
            ci_vars,
            dir,
            ci_dir: self.dir.clone(),
            started_ms: now_ms(),
            finished_ms: AtomicU64::new(0),
            status: Mutex::new("running".into()),
            cancel: AtomicBool::new(false),
            jobs,
            docker,
            notes: Mutex::new(notes),
            shared_ws: Mutex::new(None),
            driving: AtomicBool::new(false),
            build_result: Mutex::new("SUCCESS".into()),
        });
        {
            let mut runs = self.runs.lock().unwrap();
            runs.push(run.clone());
            while runs.len() > MAX_RUNS {
                let old = runs.remove(0);
                let _ = std::fs::remove_dir_all(&old.dir);
            }
        }
        drive_async(run);
        Ok(id)
    }

    fn find(&self, id: &str) -> Option<Arc<Run>> {
        self.runs.lock().unwrap().iter().find(|r| r.id == id).cloned()
    }

    pub fn list(&self) -> Vec<RunView> {
        let runs = self.runs.lock().unwrap().clone();
        runs.iter().rev().map(|r| view(r)).collect()
    }

    pub fn get(&self, id: &str) -> Option<RunView> {
        self.find(id).map(|r| view(&r))
    }

    pub fn log(&self, id: &str, job: &str, after: usize) -> Option<LogPage> {
        let run = self.find(id)?;
        let js = run.jobs.iter().find(|j| j.job.id == job)?;
        let l = js.log.lock().unwrap();
        let lines: Vec<LogLine> = l.iter().skip(after).take(4000).cloned().collect();
        Some(LogPage { next: after + lines.len(), lines })
    }

    pub fn cancel(&self, id: &str) {
        if let Some(r) = self.find(id) {
            r.cancel.store(true, Ordering::SeqCst);
            for (i, _) in r.jobs.iter().enumerate() {
                super::runner::kill_job(&r, i);
            }
        }
    }

    pub fn cancel_all(&self) {
        let ids: Vec<String> = self.runs.lock().unwrap().iter().map(|r| r.id.clone()).collect();
        for id in ids {
            self.cancel(&id);
        }
    }

    pub fn delete(&self, id: &str) {
        self.cancel(id);
        let mut runs = self.runs.lock().unwrap();
        if let Some(pos) = runs.iter().position(|r| r.id == id) {
            let r = runs.remove(pos);
            let _ = std::fs::remove_dir_all(&r.dir);
        }
    }

    pub fn run_ids(&self) -> Vec<String> {
        self.runs.lock().unwrap().iter().map(|r| r.id.clone()).collect()
    }

    /// Is any pipeline still working (or waiting to be)?
    pub fn any_running(&self) -> bool {
        self.runs.lock().unwrap().iter().any(|r| *r.status.lock().unwrap() == "running")
    }

    /// Forget every finished run and delete its files. Returns how many were removed.
    pub fn purge_finished(&self) -> usize {
        let mut runs = self.runs.lock().unwrap();
        let before = runs.len();
        runs.retain(|r| {
            let keep = *r.status.lock().unwrap() == "running";
            if !keep {
                let _ = std::fs::remove_dir_all(&r.dir);
            }
            keep
        });
        before - runs.len()
    }

    /// Run a manual job (or re-run a finished one) inside an existing run.
    pub fn play(&self, id: &str, job: &str, only_steps: Vec<usize>) -> Result<(), String> {
        let run = self.find(id).ok_or("run not found")?;
        let idx = run.jobs.iter().position(|j| j.job.id == job).ok_or("job not found")?;
        {
            let mut d = run.jobs[idx].dynamic.lock().unwrap();
            if d.status == "running" {
                return Err("this job is already running".into());
            }
            *d = JobDyn { status: "pending".into(), forced: true, only_steps, ..Default::default() };
        }
        run.jobs[idx].log.lock().unwrap().clear();
        run.cancel.store(false, Ordering::SeqCst);
        *run.status.lock().unwrap() = "running".into();
        run.finished_ms.store(0, Ordering::SeqCst);
        // Re-running a job invalidates what was downstream of it only if the user re-runs those too.
        drive_async(run);
        Ok(())
    }
}

fn job_view(run: &Run, i: usize) -> JobView {
    let js = &run.jobs[i];
    let d = js.dynamic.lock().unwrap();
    JobView {
        id: js.job.id.clone(),
        name: js.job.name.clone(),
        stage: js.job.stage.clone(),
        status: d.status.clone(),
        needs: run.deps_of(i).iter().map(|x| run.jobs[*x].job.id.clone()).collect(),
        started_ms: d.started_ms,
        finished_ms: d.finished_ms,
        attempt: d.attempt,
        note: d.note.clone(),
        allow_failure: js.job.allow_failure,
        steps: if d.steps.is_empty() {
            js.job.steps.iter().map(|s| StepView { name: s.name.clone(), kind: kind_label(&s.kind).into(), status: "pending".into(), started_ms: None, finished_ms: None }).collect()
        } else {
            d.steps.iter().map(|s| StepView { name: s.name.clone(), kind: s.kind.clone(), status: s.status.clone(), started_ms: s.started_ms, finished_ms: s.finished_ms }).collect()
        },
        artifacts: d.artifacts.clone(),
        runner: d.runner.clone(),
        log_lines: js.log.lock().unwrap().len(),
        coverage: d.coverage.clone(),
    }
}

pub fn kind_label(k: &StepKind) -> &'static str {
    match k {
        StepKind::Script { .. } => "script",
        StepKind::Uses { .. } => "action",
        StepKind::Echo { .. } => "echo",
        StepKind::Archive { .. } => "archive",
        StepKind::Stash { .. } | StepKind::Unstash { .. } => "stash",
        StepKind::Junit { .. } => "junit",
        StepKind::Sleep { .. } => "sleep",
        StepKind::Error { .. } => "error",
        StepKind::Input { .. } => "input",
        StepKind::If { .. } => "if",
        StepKind::Try { .. } => "try",
        StepKind::SetVar { .. } => "set",
        StepKind::Parallel { .. } => "parallel",
        StepKind::Unsupported { .. } => "unsupported",
    }
}

pub fn view(r: &Run) -> RunView {
    let fin = r.finished_ms.load(Ordering::SeqCst);
    RunView {
        id: r.id.clone(),
        number: r.number,
        provider: r.pipeline.provider.clone(),
        file: r.pipeline.file.clone(),
        name: r.pipeline.name.clone(),
        repo_path: r.opts.repo_path.clone(),
        status: r.status.lock().unwrap().clone(),
        started_ms: r.started_ms,
        finished_ms: if fin == 0 { None } else { Some(fin) },
        trigger: r.trig.clone(),
        source: if r.trig.working { "working".into() } else { "committed".into() },
        runner: if r.docker { "docker".into() } else { "host".into() },
        stages: r.pipeline.stages.clone(),
        jobs: (0..r.jobs.len()).map(|i| job_view(r, i)).collect(),
        warnings: r.pipeline.warnings.clone(),
        notes: r.notes.lock().unwrap().clone(),
        dir: r.dir.to_string_lossy().into_owned(),
    }
}

// ============================== scheduler ==============================

enum Decision {
    Wait,
    Run,
    Skip(String),
}

fn is_terminal(s: &str) -> bool {
    matches!(s, "passed" | "failed" | "warning" | "skipped" | "canceled")
}

fn decide(run: &Run, idx: usize) -> Decision {
    let js = &run.jobs[idx];
    let job = &js.job;
    let (forced, selected) = {
        let d = js.dynamic.lock().unwrap();
        (d.forced, d.selected)
    };
    if forced {
        return Decision::Run;
    }
    if selected {
        // Picked by the user: wait for the jobs it depends on that are running, then run whatever they ended with.
        let busy = run.deps_of(idx).iter().any(|d| matches!(run.status_of(*d).as_str(), "pending" | "running"));
        return if busy { Decision::Wait } else { Decision::Run };
    }
    let deps = run.deps_of(idx);
    let mut failed_dep = false;
    let mut all_ok = true;
    for d in &deps {
        let (st, blocking_manual) = {
            let dj = &run.jobs[*d];
            let st = dj.dynamic.lock().unwrap().status.clone();
            (st.clone(), st == "manual" && !dj.job.allow_failure)
        };
        if st == "pending" || st == "running" || blocking_manual {
            return Decision::Wait;
        }
        match st.as_str() {
            "failed" | "canceled" => {
                failed_dep = true;
                all_ok = false;
            }
            "skipped" => {
                // A skipped dependency does not count as failure for stage-ordered pipelines.
                if !run.pipeline.gated_by_stage {
                    all_ok = false;
                }
            }
            "manual" => {
                if !run.pipeline.gated_by_stage {
                    all_ok = false;
                }
            }
            _ => {}
        }
    }
    match run.pipeline.provider.as_str() {
        "github" => {
            if let Some(c) = &job.cond {
                match super::runner::gh_job_condition(run, idx, c, all_ok) {
                    Ok(true) => Decision::Run,
                    Ok(false) => Decision::Skip("`if:` condition was false".into()),
                    Err(e) => Decision::Skip(format!("could not evaluate `if:`: {e}")),
                }
            } else if all_ok {
                Decision::Run
            } else {
                Decision::Skip("a job it needs did not succeed".into())
            }
        }
        _ => {
            let by = match job.when.as_str() {
                "always" => Decision::Run,
                "on_failure" => {
                    if failed_dep {
                        Decision::Run
                    } else {
                        Decision::Skip("runs only when an earlier job failed".into())
                    }
                }
                "never" => Decision::Skip("when: never".into()),
                "manual" => Decision::Skip("manual job".into()),
                _ => {
                    if failed_dep {
                        Decision::Skip("an earlier job failed".into())
                    } else {
                        Decision::Run
                    }
                }
            };
            if let (Decision::Run, Some(c)) = (&by, &job.cond) {
                return match super::runner::jenkins_when(run, idx, c) {
                    Ok(true) => Decision::Run,
                    Ok(false) => Decision::Skip("`when` condition not met".into()),
                    Err(e) => Decision::Skip(format!("could not evaluate `when`: {e}")),
                };
            }
            by
        }
    }
}

pub fn drive_async(run: Arc<Run>) {
    if run.driving.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        drive(&run);
        run.driving.store(false, Ordering::SeqCst);
    });
}

fn drive(run: &Arc<Run>) {
    let max = run.opts.max_parallel.max(1);
    let mut handles: Vec<std::thread::JoinHandle<()>> = vec![];
    loop {
        handles.retain(|h| !h.is_finished());
        if run.cancel.load(Ordering::SeqCst) {
            for js in &run.jobs {
                let mut d = js.dynamic.lock().unwrap();
                if d.status == "pending" {
                    d.status = "canceled".into();
                    d.note = Some("run cancelled".into());
                }
            }
        }
        let running = run.jobs.iter().filter(|j| j.dynamic.lock().unwrap().status == "running").count();
        let mut started_any = false;
        let mut slots = max.saturating_sub(running);
        for i in 0..run.jobs.len() {
            if run.status_of(i) != "pending" {
                continue;
            }
            match decide(run, i) {
                Decision::Wait => {}
                Decision::Skip(why) => {
                    let mut d = run.jobs[i].dynamic.lock().unwrap();
                    d.status = "skipped".into();
                    d.note = Some(why);
                    started_any = true;
                }
                Decision::Run => {
                    if slots == 0 {
                        continue;
                    }
                    slots -= 1;
                    {
                        let mut d = run.jobs[i].dynamic.lock().unwrap();
                        d.status = "running".into();
                        d.started_ms = Some(now_ms());
                    }
                    let r = run.clone();
                    handles.push(std::thread::spawn(move || super::runner::run_job(&r, i)));
                    started_any = true;
                }
            }
        }
        let running_now = run.jobs.iter().any(|j| j.dynamic.lock().unwrap().status == "running");
        let pending_now = run.jobs.iter().any(|j| j.dynamic.lock().unwrap().status == "pending");
        if !running_now && !pending_now {
            break;
        }
        if !running_now && !started_any {
            // Pending jobs that can never start: they are waiting for a blocking manual job.
            break;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    for h in handles {
        let _ = h.join();
    }
    finalize(run);
}

fn finalize(run: &Run) {
    let statuses: Vec<String> = run.jobs.iter().map(|j| j.dynamic.lock().unwrap().status.clone()).collect();
    let status = if run.cancel.load(Ordering::SeqCst) {
        "canceled"
    } else if statuses.iter().any(|s| s == "failed") {
        "failed"
    } else if statuses.iter().any(|s| s == "pending") {
        "blocked"
    } else if statuses.iter().any(|s| s == "warning") {
        "warning"
    } else if statuses.iter().all(|s| s == "skipped" || s == "manual") && statuses.iter().any(|s| s == "manual") {
        "blocked"
    } else {
        "passed"
    };
    let _ = is_terminal("passed");
    *run.status.lock().unwrap() = status.to_string();
    run.finished_ms.store(now_ms(), Ordering::SeqCst);
    cleanup_run_containers(run);
}

fn cleanup_run_containers(run: &Run) {
    for i in 0..run.jobs.len() {
        super::runner::kill_job(run, i);
    }
}

pub fn job_dir_name(job_id: &str) -> String {
    sanitize_name(job_id)
}

pub fn path_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub fn env_map(list: &[Kv]) -> HashMap<String, String> {
    list.iter().map(|k| (k.key.clone(), k.value.clone())).collect()
}
