//! Provider-neutral pipeline model. GitLab CI, GitHub Actions and Jenkins files are all parsed into this,
//! and one engine runs it.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Kv {
    pub key: String,
    pub value: String,
}

impl Kv {
    pub fn new(k: impl Into<String>, v: impl Into<String>) -> Kv {
        Kv { key: k.into(), value: v.into() }
    }
}

/// Insert or overwrite `key` in an ordered key/value list.
pub fn put(list: &mut Vec<Kv>, key: &str, value: impl Into<String>) {
    let value = value.into();
    if let Some(e) = list.iter_mut().find(|e| e.key == key) {
        e.value = value;
    } else {
        list.push(Kv::new(key, value));
    }
}

pub fn get<'a>(list: &'a [Kv], key: &str) -> Option<&'a str> {
    list.iter().find(|e| e.key == key).map(|e| e.value.as_str())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDef {
    pub image: String,
    /// Network alias the job reaches the service by (`postgres`, or the key in a GitHub `services:` map).
    pub alias: String,
    pub env: Vec<Kv>,
    pub command: Option<String>,
    /// Extra `docker run` flags (GitHub `options:` e.g. `--health-cmd ...`).
    pub options: Vec<String>,
    pub ports: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Artifacts {
    pub paths: Vec<String>,
    pub exclude: Vec<String>,
    /// on_success | on_failure | always
    pub when: String,
    pub name: Option<String>,
    pub dotenv: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CacheDef {
    pub key: String,
    pub paths: Vec<String>,
    /// pull-push | pull | push
    pub policy: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    /// GitLab expression (`$CI_COMMIT_BRANCH == "main"`).
    pub cond: Option<String>,
    pub changes: Vec<String>,
    pub exists: Vec<String>,
    /// Override of the job's `when` when this rule matches ("never" excludes the job).
    pub when: Option<String>,
    pub allow_failure: Option<bool>,
    pub variables: Vec<Kv>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Need {
    pub job: String,
    pub artifacts: bool,
    pub optional: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum StepKind {
    /// A shell script. `style` picks how it is wrapped: gitlab | github | jenkins | plain.
    Script { script: String, style: String, interpolate: bool },
    /// GitHub `uses:`.
    Uses { action: String, with: Vec<Kv> },
    Echo { text: String, interpolate: bool },
    Archive { patterns: String, allow_empty: bool },
    Stash { name: String, includes: String },
    Unstash { name: String },
    Junit { pattern: String },
    Sleep { seconds: u64 },
    Error { message: String },
    /// Jenkins `input`: auto-approved locally, with a note in the log.
    Input { message: String },
    /// Jenkins `if (...) { } else { }` inside `script {}`; the condition is a Groovy expression.
    If { cond: String, then: Vec<Step>, otherwise: Vec<Step> },
    /// Jenkins `try { } catch { } finally { }`.
    Try { body: Vec<Step>, catch_: Vec<Step>, finally: Vec<Step> },
    /// Jenkins `env.X = ...`, `def x = ...`, `currentBuild.result = ...`. `script` = capture `sh(returnStdout: true)`.
    SetVar { key: String, expr: String, script: Option<String>, env: bool },
    /// Jenkins `parallel a: {...}, b: {...}` inside a script block (branches run one after another).
    Parallel { branches: Vec<(String, Vec<Step>)> },
    /// Something the simulator does not run. Executing it fails the step, loudly.
    Unsupported { what: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub name: String,
    pub id: Option<String>,
    pub kind: StepKind,
    pub env: Vec<Kv>,
    /// Working directory relative to the project dir.
    pub dir: Option<String>,
    /// GitHub `if:` expression.
    pub cond: Option<String>,
    /// Runs even when an earlier step failed (GitLab `after_script`).
    pub always: bool,
    pub continue_on_error: bool,
    /// Jenkins `catchError`: failure marks the build but the pipeline carries on.
    pub catch_error: bool,
    pub retry: u32,
    pub timeout_secs: Option<u64>,
    pub shell: Option<String>,
}

impl Step {
    pub fn new(name: impl Into<String>, kind: StepKind) -> Step {
        Step {
            name: name.into(),
            id: None,
            kind,
            env: vec![],
            dir: None,
            cond: None,
            always: false,
            continue_on_error: false,
            catch_error: false,
            retry: 0,
            timeout_secs: None,
            shell: None,
        }
    }

    pub fn script(name: impl Into<String>, script: impl Into<String>, style: &str) -> Step {
        Step::new(name, StepKind::Script { script: script.into(), style: style.into(), interpolate: false })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PostBlock {
    /// always | success | failure | unstable | changed | cleanup | fixed | regression | aborted
    pub cond: String,
    pub steps: Vec<Step>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub name: String,
    pub stage: String,
    pub needs: Vec<Need>,
    /// `needs` was given explicitly (so stage ordering does not apply to this job).
    pub has_needs: bool,
    pub image: Option<String>,
    /// Extra `docker run` flags for the job container (Jenkins `args`, GitHub `container.options`).
    pub docker_args: Vec<String>,
    pub services: Vec<ServiceDef>,
    pub env: Vec<Kv>,
    pub steps: Vec<Step>,
    /// on_success | on_failure | always | manual | never
    pub when: String,
    pub allow_failure: bool,
    /// GitHub job-level `if:`.
    pub cond: Option<String>,
    pub rules: Vec<Rule>,
    pub retry: u32,
    pub timeout_secs: Option<u64>,
    pub artifacts: Option<Artifacts>,
    pub cache: Vec<CacheDef>,
    /// `Some(list)` = only fetch artifacts from these jobs.
    pub dependencies: Option<Vec<String>>,
    pub matrix: Vec<Kv>,
    pub runs_on: Option<String>,
    pub post: Vec<PostBlock>,
    /// GitHub job `outputs:` (expressions evaluated at the end of the job).
    pub outputs: Vec<Kv>,
    /// GitHub job `defaults.run.shell` / `working-directory`.
    pub default_shell: Option<String>,
    pub default_dir: Option<String>,
    /// Jenkins: this stage uses the pipeline-wide workspace.
    pub shared_workspace: bool,
    /// GitLab `parallel: N` -> CI_NODE_INDEX / CI_NODE_TOTAL.
    pub node: Option<(u32, u32)>,
    pub coverage: Option<String>,
    pub trigger: bool,
    pub manual_note: Option<String>,
}

impl Job {
    pub fn new(id: impl Into<String>, stage: impl Into<String>) -> Job {
        let id = id.into();
        Job { name: id.clone(), id, stage: stage.into(), when: "on_success".into(), ..Default::default() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    pub name: String,
    pub default: String,
    pub description: String,
    /// string | boolean | choice
    pub kind: String,
    pub choices: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pipeline {
    /// gitlab | github | jenkins
    pub provider: String,
    pub file: String,
    pub name: String,
    pub stages: Vec<String>,
    pub jobs: Vec<Job>,
    /// Pipeline-wide variables (GitLab `variables`, GitHub top-level `env`, Jenkins `environment`).
    pub env: Vec<Kv>,
    pub warnings: Vec<String>,
    /// Jobs wait for the whole previous stage (GitLab, Jenkins) instead of only their `needs` (GitHub).
    pub gated_by_stage: bool,
    pub post: Vec<PostBlock>,
    pub params: Vec<Param>,
    /// GitHub `on:` events.
    pub events: Vec<String>,
    pub default_image: Option<String>,
    /// Jenkins top-level agent image/args.
    pub default_docker_args: Vec<String>,
    /// Jenkins `agent none`/`any`/label means "run on the machine".
    pub host_agent: bool,
    /// GitHub `defaults.run`.
    pub default_shell: Option<String>,
    pub default_dir: Option<String>,
}

/// Files that define a pipeline in a repo, discovered by `discover`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PipelineFile {
    pub provider: String,
    pub file: String,
}
