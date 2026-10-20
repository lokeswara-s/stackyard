use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::engine::*;
use super::expr::{gh_condition, gh_eval, gh_interpolate, gl_eval, GhCtx};
use super::groovy::{self, Scope, GV};
use super::ir::*;
use super::{discover, gitlab, jenkins};
use crate::model::EnvVar;
use crate::util::{now_ms, run};

fn temp_repo(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dwci-t-{name}-{}", now_ms()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn git_init(dir: &Path) {
    let d = dir.to_string_lossy().to_string();
    for args in [vec!["init", "-q", "-b", "main"], vec!["config", "user.email", "t@t.t"], vec!["config", "user.name", "Tester"], vec!["add", "-A"], vec!["commit", "-q", "-m", "initial commit"]] {
        let o = run("git", &args, Some(&d)).unwrap();
        assert!(o.ok, "git {:?}: {}", args, o.error_text());
    }
}

fn start(ci: &Arc<Ci>, repo: &Path, provider: &str, file: &str, tweak: impl FnOnce(&mut RunOptions)) -> String {
    let mut o = RunOptions { provider: provider.into(), file: file.into(), repo_path: repo.to_string_lossy().into_owned(), runner: "host".into(), ..Default::default() };
    tweak(&mut o);
    ci.start(o, vec![]).expect("start")
}

fn wait(ci: &Arc<Ci>, id: &str, secs: u64) -> RunView {
    let t = Instant::now();
    loop {
        let v = ci.get(id).unwrap();
        if v.status != "running" {
            return v;
        }
        if t.elapsed() > Duration::from_secs(secs) {
            ci.cancel(id);
            panic!("pipeline did not finish in {secs}s:\n{}", dump(ci, &v));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn log_of(ci: &Arc<Ci>, id: &str, job: &str) -> String {
    ci.log(id, job, 0).map(|p| p.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
}

fn dump(ci: &Arc<Ci>, v: &RunView) -> String {
    let mut s = format!("run {} status={}\n", v.id, v.status);
    for j in &v.jobs {
        s.push_str(&format!("--- {} [{}] note={:?}\n{}\n", j.id, j.status, j.note, log_of(ci, &v.id, &j.id)));
    }
    s
}

fn job<'a>(v: &'a RunView, id: &str) -> &'a JobView {
    v.jobs.iter().find(|j| j.id == id).unwrap_or_else(|| panic!("no job `{id}` in {:?}", v.jobs.iter().map(|j| j.id.clone()).collect::<Vec<_>>()))
}

fn new_ci() -> Arc<Ci> {
    Arc::new(Ci::new(temp_repo("ci-data")))
}

// ============================== parsing ==============================

#[test]
fn gitlab_parser_handles_extends_anchors_reference_matrix_and_rules() {
    let repo = temp_repo("gl-parse");
    write(&repo, "ci/inc.yml", "lint:\n  stage: test\n  script: [echo lint]\n");
    write(
        &repo,
        ".gitlab-ci.yml",
        r#"
include:
  - local: ci/inc.yml
  - project: other/group
    file: x.yml
stages: [build, test, deploy]
variables: { A: "1" }
.setup: &setup
  - echo setup
.tpl:
  image: node:20
  before_script: [echo tpl-before]
  variables: { FROM_TPL: yes }
build:
  extends: .tpl
  stage: build
  script:
    - !reference [.setup]
    - echo build
  artifacts:
    paths: [dist/]
    when: always
    reports: { dotenv: b.env }
  cache:
    key: { files: [package-lock.json], prefix: deps }
    paths: [node_modules/]
unit:
  stage: test
  needs: [build]
  parallel:
    matrix:
      - OS: [linux, mac]
        V: ["1", "2"]
  script: echo $OS $V
deploy:
  stage: deploy
  script: echo deploy
  when: manual
  timeout: 1h 30m
  retry: { max: 2 }
  only: [main, /^release-.*/]
  except: [tags]
"#,
    );
    let p = gitlab::parse(&repo, ".gitlab-ci.yml").unwrap();
    assert_eq!(p.stages, vec!["build", "test", "deploy"]);
    let ids: Vec<&str> = p.jobs.iter().map(|j| j.id.as_str()).collect();
    assert!(ids.contains(&"lint") && ids.contains(&"build") && ids.contains(&"deploy"), "{ids:?}");
    assert_eq!(p.jobs.iter().filter(|j| j.id.starts_with("unit: [")).count(), 4);
    let build = p.jobs.iter().find(|j| j.id == "build").unwrap();
    assert_eq!(build.image.as_deref(), Some("node:20"));
    let StepKind::Script { script, .. } = &build.steps[0].kind else { panic!() };
    assert!(script.contains("echo tpl-before") && script.contains("echo setup") && script.contains("echo build"), "{script:?}");
    assert_eq!(build.artifacts.as_ref().unwrap().dotenv.as_deref(), Some("b.env"));
    assert!(build.cache[0].key.starts_with("files:package-lock.json"));
    let deploy = p.jobs.iter().find(|j| j.id == "deploy").unwrap();
    assert_eq!(deploy.timeout_secs, Some(5400));
    assert_eq!(deploy.retry, 2);
    assert_eq!(deploy.when, "manual");
    assert!(deploy.rules.len() >= 2, "only/except become rules: {:?}", deploy.rules);
    assert!(p.warnings.iter().any(|w| w.contains("cannot be fetched")), "{:?}", p.warnings);
    assert_eq!(p.jobs.iter().find(|j| j.id == "unit: [linux, 1]").unwrap().env.iter().find(|e| e.key == "OS").unwrap().value, "linux");
}

#[test]
fn github_parser_expands_matrix_with_include_exclude_services_and_container() {
    let repo = temp_repo("gh-parse");
    write(
        &repo,
        ".github/workflows/ci.yml",
        r#"
name: CI
on:
  push:
  workflow_dispatch:
    inputs:
      who: { description: Who, default: world }
env: { TOP: t }
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
        node: [18, 20]
        exclude:
          - os: windows-latest
            node: 18
        include:
          - os: ubuntu-latest
            node: 20
            experimental: true
          - os: macos-latest
            node: 22
    services:
      db:
        image: postgres:16
        env: { POSTGRES_PASSWORD: pw }
        options: --health-cmd "pg_isready" --health-interval 5s
    steps:
      - uses: actions/checkout@v4
      - run: echo hi
        working-directory: sub
        shell: bash
        continue-on-error: true
  deploy:
    needs: [test]
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    container: { image: node:20, options: --user root }
    steps:
      - run: echo deploy
"#,
    );
    let p = super::github::parse(&repo, ".github/workflows/ci.yml").unwrap();
    // 2x2 - 1 excluded + 1 new combination (macos) = 4 jobs, and the first gets `experimental`.
    let tests: Vec<&Job> = p.jobs.iter().filter(|j| j.id.starts_with("test (")).collect();
    assert_eq!(tests.len(), 4, "{:?}", tests.iter().map(|j| &j.id).collect::<Vec<_>>());
    assert!(tests.iter().any(|j| j.matrix.iter().any(|k| k.key == "experimental")));
    assert!(tests.iter().any(|j| j.matrix.iter().any(|k| k.key == "os" && k.value == "\"macos-latest\"")));
    assert_eq!(tests[0].services[0].alias, "db");
    assert_eq!(tests[0].services[0].options, vec!["--health-cmd", "pg_isready", "--health-interval", "5s"]);
    assert_eq!(p.params[0].name, "who");
    let d = p.jobs.iter().find(|j| j.id == "deploy").unwrap();
    assert_eq!(d.image.as_deref(), Some("node:20"));
    assert_eq!(d.docker_args, vec!["--user", "root"]);
    assert_eq!(d.needs[0].job, "test");
    assert!(tests[0].steps[1].continue_on_error && tests[0].steps[1].dir.as_deref() == Some("sub"));
    assert!(p.events.contains(&"push".to_string()) && p.events.contains(&"workflow_dispatch".to_string()));
}

#[test]
fn jenkins_parser_reads_declarative_pipelines_with_parallel_when_post_and_script() {
    let repo = temp_repo("jk-parse");
    write(
        &repo,
        "Jenkinsfile",
        r#"
pipeline {
  agent {
    docker {
      image 'node:20-alpine'
      args '-v /tmp:/tmp'
    }
  }
  options { timeout(time: 10, unit: 'MINUTES') }
  parameters {
    string(name: 'WHO', defaultValue: 'world', description: 'x')
    booleanParam(name: 'LOUD', defaultValue: false)
    choice(name: 'ENV', choices: ['dev', 'prod'])
  }
  environment {
    A = 'plain'
    B = "dq-${env.A}"
    TOKEN = credentials('my-token')
  }
  stages {
    stage('Build') {
      steps {
        sh 'npm ci'
        sh """
          echo ${env.A}
        """
        dir('sub') { sh 'ls' }
        retry(3) { sh 'flaky' }
        script {
          if (env.A == 'plain') { echo 'yes' } else { echo 'no' }
          env.X = 'y'
        }
        withCredentials([string(credentialsId: 'tok', variable: 'T')]) { sh 'echo $T' }
        archiveArtifacts artifacts: 'dist/**', allowEmptyArchive: true
      }
    }
    stage('Tests') {
      parallel {
        stage('Unit') { steps { sh 'npm test' } }
        stage('Lint') { steps { sh 'npm run lint' } }
      }
    }
    stage('Deploy') {
      when { allOf { branch 'main'; not { environment name: 'SKIP', value: '1' } } }
      steps { echo 'deploying' }
    }
  }
  post {
    always { echo 'done' }
    failure { echo 'failed' }
  }
}
"#,
    );
    let p = jenkins::parse(&repo, "Jenkinsfile").unwrap();
    assert_eq!(p.stages, vec!["Build", "Tests", "Deploy", "post"]);
    let ids: Vec<&str> = p.jobs.iter().map(|j| j.id.as_str()).collect();
    assert_eq!(ids, vec!["Build", "Tests / Unit", "Tests / Lint", "Deploy", "post actions"]);
    assert_eq!(p.default_image.as_deref(), Some("node:20-alpine"));
    assert_eq!(p.jobs[0].docker_args, vec!["-v", "/tmp:/tmp"]);
    assert_eq!(p.params.len(), 3);
    assert_eq!(p.params[2].choices, vec!["dev", "prod"]);
    assert!(p.env.iter().find(|e| e.key == "B").unwrap().value.starts_with(jenkins::GSTR));
    assert!(p.env.iter().find(|e| e.key == "TOKEN").unwrap().value.starts_with(jenkins::CRED));
    let build = &p.jobs[0];
    assert_eq!(build.timeout_secs, Some(600));
    assert_eq!(build.steps.len(), 8, "{:?}", build.steps.iter().map(|s| &s.name).collect::<Vec<_>>());
    assert_eq!(build.steps[2].dir.as_deref(), Some("sub"));
    assert_eq!(build.steps[3].retry, 2);
    assert!(matches!(build.steps[4].kind, StepKind::If { .. }) || matches!(build.steps[4].kind, StepKind::SetVar { .. }));
    let deploy = &p.jobs[3];
    let cond = deploy.cond.clone().unwrap();
    assert!(cond.contains("__branch('main')") && cond.contains("!(") && cond.contains("env.SKIP"), "{cond}");
    assert_eq!(p.jobs[4].post.len(), 2);
}

// ============================== expressions ==============================

fn hf(_: &[String]) -> String {
    "HASH".into()
}

#[test]
fn github_expressions_match_the_documented_semantics() {
    let root = serde_json::json!({
        "github": {"ref": "refs/heads/main", "event_name": "push", "sha": "abc"},
        "matrix": {"node": 20, "os": "Ubuntu"},
        "steps": {"a": {"outputs": {"v": "1.2"}, "outcome": "success"}},
        "needs": {"build": {"result": "success", "outputs": {"x": "y"}}},
        "env": {"E": "val"},
    });
    let cx = GhCtx { root: &root, ok: true, cancelled: false, hash_files: &hf };
    let ev = |e: &str| gh_eval(e, &cx).unwrap();
    assert_eq!(ev("github.ref == 'refs/heads/main'"), serde_json::json!(true));
    assert_eq!(ev("matrix.node >= 18 && matrix.os == 'ubuntu'"), serde_json::json!(true));
    assert_eq!(ev("contains(github.ref, 'heads') && !startsWith(github.ref, 'refs/tags')"), serde_json::json!(true));
    assert_eq!(ev("steps.a.outputs.v"), serde_json::json!("1.2"));
    assert_eq!(ev("format('{0}-{1}', env.E, matrix.node)"), serde_json::json!("val-20"));
    assert_eq!(ev("needs.build.result == 'success' || false"), serde_json::json!(true));
    assert_eq!(ev("fromJSON('{\"a\": [1,2]}').a[1]").as_f64(), Some(2.0));
    assert_eq!(ev("hashFiles('**/x')"), serde_json::json!("HASH"));
    assert_eq!(ev("matrix.missing || 'dflt'"), serde_json::json!("dflt"));
    assert_eq!(gh_interpolate("v=${{ steps.a.outputs.v }} n=${{ matrix.node }} lit=${{ 'a''b' }}", &cx).unwrap(), "v=1.2 n=20 lit=a'b");
    assert!(gh_condition("${{ success() && github.event_name == 'push' }}", &cx).unwrap());
    assert!(!gh_condition("failure()", &cx).unwrap());
    let bad = GhCtx { ok: false, ..GhCtx { root: &root, ok: true, cancelled: false, hash_files: &hf } };
    assert!(gh_condition("failure()", &bad).unwrap() && gh_condition("always()", &bad).unwrap() && !gh_condition("success()", &bad).unwrap());
    assert!(gh_eval("nope(1)", &cx).is_err());
}

#[test]
fn gitlab_rule_expressions_evaluate_variables_regex_and_null() {
    let vars = |n: &str| match n {
        "CI_COMMIT_BRANCH" => Some("release-1.2".to_string()),
        "CI_PIPELINE_SOURCE" => Some("push".to_string()),
        "EMPTY" => Some(String::new()),
        _ => None,
    };
    assert!(gl_eval(r#"$CI_COMMIT_BRANCH =~ /^release-\d+\.\d+$/"#, &vars).unwrap());
    assert!(gl_eval(r#"$CI_COMMIT_BRANCH == "main" || $CI_PIPELINE_SOURCE == "push""#, &vars).unwrap());
    assert!(gl_eval(r#"$CI_COMMIT_TAG == null && $CI_COMMIT_BRANCH"#, &vars).unwrap());
    assert!(!gl_eval(r#"$CI_COMMIT_BRANCH !~ /release/"#, &vars).unwrap());
    assert!(!gl_eval(r#"$EMPTY"#, &vars).unwrap());
    assert!(gl_eval(r#"($CI_COMMIT_BRANCH == "x" || $CI_COMMIT_BRANCH =~ /REL/i) && $CI_PIPELINE_SOURCE != "web""#, &vars).unwrap());
    assert!(gl_eval("$A ==", &vars).is_err());
}

struct TS;
impl Scope for TS {
    fn var(&self, n: &str) -> Option<String> {
        match n {
            "env.NAME" | "NAME" => Some("jenkins".into()),
            "params.N" => Some("3".into()),
            "env.BRANCH_NAME" => Some("feature/x".into()),
            _ => None,
        }
    }
    fn call(&self, n: &str, a: &[GV]) -> Option<GV> {
        (n == "__branch").then(|| GV::Bool(groovy::glob(&a[0].text(), "feature/x")))
    }
}

#[test]
fn groovy_expressions_and_gstrings() {
    let ev = |e: &str| groovy::eval(e, &TS).unwrap().text();
    assert_eq!(ev("env.NAME == 'jenkins' && params.N.toInteger() > 2"), "true");
    assert_eq!(ev("env.NAME.toUpperCase() + '-' + params.N"), "JENKINS-3");
    assert_eq!(ev("env.BRANCH_NAME ==~ /feature\\/.*/"), "true");
    assert_eq!(ev("params.N == '3' ? 'three' : 'other'"), "three");
    assert_eq!(ev("__branch('feature/*')"), "true");
    assert_eq!(ev("env.MISSING ?: 'fallback'"), "fallback");
    assert_eq!(ev("['a','b'].contains('b')"), "true");
    assert_eq!(groovy::interpolate("hi ${env.NAME} $NAME ${params.N + 1} \u{1}HOME $UNKNOWN", &TS), "hi jenkins jenkins 31 $HOME $UNKNOWN");
}

#[test]
fn discovers_pipeline_files_in_a_repo() {
    let repo = temp_repo("discover");
    write(&repo, ".gitlab-ci.yml", "a:\n  script: x\n");
    write(&repo, ".github/workflows/b.yml", "on: push\njobs: {}\n");
    write(&repo, ".github/workflows/a.yaml", "on: push\njobs: {}\n");
    write(&repo, "Jenkinsfile", "pipeline {}\n");
    let f: Vec<(String, String)> = discover(&repo).into_iter().map(|p| (p.provider, p.file)).collect();
    assert_eq!(f, vec![("gitlab".into(), ".gitlab-ci.yml".into()), ("github".into(), ".github/workflows/a.yaml".into()), ("github".into(), ".github/workflows/b.yml".into()), ("jenkins".into(), "Jenkinsfile".into())]);
}

// ============================== real runs (on this machine) ==============================

#[test]
fn gitlab_pipeline_runs_stages_artifacts_dotenv_matrix_rules_and_after_script() {
    let repo = temp_repo("gl-run");
    write(
        &repo,
        ".gitlab-ci.yml",
        r#"
stages: [build, test, deploy]
variables:
  GREETING: hello
  COMPOSED: "$GREETING-world"
.base:
  before_script:
    - echo "before-script ran"
build:
  stage: build
  extends: .base
  script:
    - echo "building $CI_COMMIT_REF_NAME $GREETING on $CI_PIPELINE_SOURCE"
    - mkdir -p dist && echo artifact-content > dist/out.txt
    - echo "VERSION=1.2.3" > build.env
    - |
      echo multi
      echo line
  artifacts:
    paths: [dist/]
    reports:
      dotenv: build.env
test:
  stage: test
  needs: [build]
  script:
    - cat dist/out.txt
    - echo "version=$VERSION composed=$COMPOSED"
unit:
  stage: test
  parallel:
    matrix:
      - K: [a, b]
  script: echo "K=$K node=$CI_NODE_TOTAL"
never:
  stage: deploy
  script: echo SHOULD_NOT_RUN
  rules:
    - if: '$CI_COMMIT_BRANCH == "nope"'
flaky-allowed:
  stage: deploy
  script: exit 3
  allow_failure: true
final:
  stage: deploy
  script: echo hi
  after_script:
    - echo "after-script ran"
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |_| {});
    let v = wait(&ci, &id, 180);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "warning", "{d}");
    assert_eq!(job(&v, "build").status, "passed", "{d}");
    let b = log_of(&ci, &id, "build");
    assert!(b.contains("before-script ran") && b.contains("$ echo \"building") && b.contains("building main hello on push"), "{b}");
    assert!(b.contains("$ echo multi") && b.contains("Uploading artifacts"), "{b}");
    assert_eq!(job(&v, "test").status, "passed", "{d}");
    let t = log_of(&ci, &id, "test");
    assert!(t.contains("artifact-content") && t.contains("version=1.2.3 composed=hello-world"), "{t}");
    assert_eq!(job(&v, "unit: [a]").status, "passed", "{d}");
    assert!(log_of(&ci, &id, "unit: [b]").contains("K=b node=1"), "{d}");
    assert_eq!(job(&v, "never").status, "skipped");
    assert_eq!(job(&v, "flaky-allowed").status, "warning", "{d}");
    assert!(log_of(&ci, &id, "final").contains("after-script ran"), "{d}");
}

#[test]
fn gitlab_failure_skips_later_stages_but_runs_on_failure_and_always_jobs() {
    let repo = temp_repo("gl-fail");
    write(
        &repo,
        ".gitlab-ci.yml",
        r#"
stages: [one, two, three]
boom:
  stage: one
  script: [echo before-boom, "false", echo not-reached]
skipped:
  stage: two
  script: echo no
cleanup:
  stage: three
  when: always
  script: echo cleanup
notify:
  stage: three
  when: on_failure
  script: echo notify
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |_| {});
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "failed", "{d}");
    assert_eq!(job(&v, "boom").status, "failed");
    assert!(!log_of(&ci, &id, "boom").contains("not-reached"), "{d}");
    assert!(log_of(&ci, &id, "boom").contains("ERROR: Job failed"));
    assert_eq!(job(&v, "skipped").status, "skipped");
    assert_eq!(job(&v, "cleanup").status, "passed", "{d}");
    assert_eq!(job(&v, "notify").status, "passed", "{d}");
}

#[test]
fn gitlab_runs_the_committed_code_not_the_working_tree_unless_asked() {
    let repo = temp_repo("gl-src");
    write(&repo, "file.txt", "committed\n");
    write(&repo, ".gitlab-ci.yml", "show:\n  script:\n    - cat file.txt\n    - 'echo \"u=$([ -f untracked.txt ] && echo Y || echo N)\"'\n");
    git_init(&repo);
    write(&repo, "file.txt", "dirty\n");
    write(&repo, "untracked.txt", "u\n");
    let ci = new_ci();
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |_| {});
    let v = wait(&ci, &id, 120);
    let l = log_of(&ci, &id, "show");
    assert!(l.contains("committed") && !l.contains("dirty") && l.contains("u=N"), "{l}");
    assert!(!v.trigger.working);
    let id2 = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |o| o.source = "working".into());
    let v2 = wait(&ci, &id2, 120);
    let l2 = log_of(&ci, &id2, "show");
    assert!(l2.contains("dirty") && l2.contains("u=Y"), "{l2}");
    assert!(v2.trigger.working);
}

#[test]
fn github_workflow_runs_outputs_env_files_conditions_matrix_needs_and_artifacts() {
    let repo = temp_repo("gh-run");
    write(
        &repo,
        ".github/workflows/ci.yml",
        r#"
name: CI
on: [push]
env:
  TOP: top-level
jobs:
  build:
    runs-on: ubuntu-latest
    outputs:
      ver: ${{ steps.v.outputs.ver }}
    steps:
      - uses: actions/checkout@v4
      - id: v
        run: echo "ver=9.9.9" >> "$GITHUB_OUTPUT"
      - run: echo "top=$TOP ver=${{ steps.v.outputs.ver }} ref=${{ github.ref_name }} actor=$GITHUB_ACTIONS"
      - name: skipped one
        if: ${{ github.ref_name == 'nope' }}
        run: echo SHOULD_NOT_PRINT
      - run: echo "EXTRA=from-env-file" >> "$GITHUB_ENV"
      - run: echo "extra=$EXTRA"
      - run: |
          mkdir -p out
          echo uploaded > out/a.txt
      - uses: actions/upload-artifact@v4
        with:
          name: dist
          path: out/a.txt
      - id: bad
        run: exit 1
        continue-on-error: true
      - run: echo "bad outcome=${{ steps.bad.outcome }} conclusion=${{ steps.bad.conclusion }}"
      - if: always()
        run: echo "always runs"
  test:
    needs: build
    runs-on: ubuntu-latest
    strategy:
      matrix:
        node: [18, 20]
    steps:
      - run: echo "node=${{ matrix.node }} needs=${{ needs.build.outputs.ver }}"
      - uses: actions/download-artifact@v4
        with:
          name: dist
      - run: cat a.txt
  never:
    needs: build
    if: github.ref_name == 'nope'
    runs-on: ubuntu-latest
    steps:
      - run: echo SHOULD_NOT_RUN
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "github", ".github/workflows/ci.yml", |_| {});
    let v = wait(&ci, &id, 180);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "passed", "{d}");
    let b = log_of(&ci, &id, "build");
    assert!(b.contains("top=top-level ver=9.9.9 ref=main actor=true"), "{d}");
    assert!(!b.contains("SHOULD_NOT_PRINT"), "{d}");
    assert!(b.contains("extra=from-env-file") && b.contains("Uploaded artifact `dist`"), "{d}");
    assert!(b.contains("bad outcome=failure conclusion=success") && b.contains("always runs"), "{d}");
    assert_eq!(job(&v, "never").status, "skipped");
    for n in ["test (18)", "test (20)"] {
        let l = log_of(&ci, &id, n);
        assert_eq!(job(&v, n).status, "passed", "{d}");
        assert!(l.contains("needs=9.9.9") && l.contains("uploaded"), "{n}: {l}");
    }
}

#[test]
fn picked_jobs_run_despite_rules_and_manual_and_can_pull_in_their_needs() {
    let repo = temp_repo("gl-pick");
    write(
        &repo,
        ".gitlab-ci.yml",
        r#"
stages: [build, deploy]
build:
  stage: build
  script: [echo building, "echo ART > art.txt"]
  artifacts: { paths: [art.txt] }
only-prod:
  stage: deploy
  script: [cat art.txt || echo no-artifact, echo deploying-now]
  rules:
    - if: '$CI_COMMIT_BRANCH == "prod"'
manual-one:
  stage: deploy
  when: manual
  script: echo manual-ran
other:
  stage: deploy
  script: echo SHOULD_NOT_RUN
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    // Normal run: the rules-excluded job is skipped (and says so), the manual one waits.
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |_| {});
    let v = wait(&ci, &id, 120);
    assert_eq!(job(&v, "only-prod").status, "skipped");
    assert!(job(&v, "only-prod").note.as_deref().unwrap_or("").contains("pick the job"), "{:?}", job(&v, "only-prod").note);
    assert_eq!(job(&v, "manual-one").status, "manual");
    // Picked: both run, the rest are "not selected", and without needs there is no artifact.
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |o| o.only_jobs = vec!["only-prod".into(), "manual-one".into()]);
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "passed", "{d}");
    assert_eq!(job(&v, "only-prod").status, "passed", "{d}");
    assert_eq!(job(&v, "manual-one").status, "passed", "{d}");
    assert_eq!(job(&v, "build").status, "skipped");
    assert_eq!(job(&v, "other").status, "skipped");
    assert!(log_of(&ci, &id, "only-prod").contains("no-artifact"), "{d}");
    // Picked with the jobs it needs: `build` runs first and its artifact is there.
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |o| {
        o.only_jobs = vec!["only-prod".into()];
        o.with_needs = true;
    });
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    assert_eq!(job(&v, "build").status, "passed", "{d}");
    assert_eq!(job(&v, "only-prod").status, "passed", "{d}");
    assert_eq!(job(&v, "other").status, "skipped");
    assert!(log_of(&ci, &id, "only-prod").contains("ART"), "{d}");
    // A skipped job can be started afterwards (`play`), optionally with only some steps.
    ci.play(&id, "other", vec![]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let v = wait(&ci, &id, 120);
    assert_eq!(job(&v, "other").status, "passed", "{}", dump(&ci, &v));
}

#[test]
fn only_the_picked_steps_of_a_job_run() {
    let repo = temp_repo("gh-steps");
    write(
        &repo,
        ".github/workflows/s.yml",
        "on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo STEP-ZERO\n      - run: echo STEP-ONE\n      - run: echo STEP-TWO\n  b:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo JOB-B\n",
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "github", ".github/workflows/s.yml", |o| {
        o.only_jobs = vec!["a".into()];
        o.only_steps = vec![1];
    });
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    let l = log_of(&ci, &id, "a");
    assert_eq!(job(&v, "a").status, "passed", "{d}");
    assert!(l.contains("STEP-ONE") && !l.contains("\nSTEP-ZERO") && !l.contains("\nSTEP-TWO"), "{d}");
    assert_eq!(job(&v, "a").steps[0].status, "skipped");
    assert_eq!(job(&v, "a").steps[1].status, "passed");
    assert_eq!(job(&v, "b").status, "skipped");
    // Run job b on its own from the finished run.
    ci.play(&id, "b", vec![]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let v = wait(&ci, &id, 120);
    assert!(log_of(&ci, &id, "b").contains("JOB-B"), "{}", dump(&ci, &v));
}

#[test]
fn github_failed_job_skips_dependents_unless_they_ask_for_failure() {
    let repo = temp_repo("gh-fail");
    write(
        &repo,
        ".github/workflows/ci.yml",
        r#"
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      - run: exit 2
  b:
    needs: a
    runs-on: ubuntu-latest
    steps:
      - run: echo SHOULD_NOT_RUN
  c:
    needs: a
    if: failure()
    runs-on: ubuntu-latest
    steps:
      - run: echo "a was ${{ needs.a.result }}"
  d:
    needs: a
    if: github.ref_name == 'main'
    runs-on: ubuntu-latest
    steps:
      - run: echo SHOULD_NOT_RUN_EITHER
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "github", ".github/workflows/ci.yml", |_| {});
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "failed");
    assert_eq!(job(&v, "b").status, "skipped", "{d}");
    assert_eq!(job(&v, "c").status, "passed", "{d}");
    assert!(log_of(&ci, &id, "c").contains("a was failure"), "{d}");
    // An `if:` without a status function implies success() (so a failed dependency still skips the job).
    assert_eq!(job(&v, "d").status, "skipped", "{d}");
}

#[test]
fn jenkins_pipeline_runs_params_env_script_blocks_when_parallel_and_post() {
    let repo = temp_repo("jk-run");
    write(
        &repo,
        "Jenkinsfile",
        r#"
pipeline {
  agent any
  parameters {
    string(name: 'WHO', defaultValue: 'world', description: '')
    booleanParam(name: 'LOUD', defaultValue: false)
  }
  environment {
    GREETING = "Hello ${params.WHO}"
    PLAIN = 'x'
    SECRET = credentials('my-secret')
  }
  stages {
    stage('Build') {
      steps {
        echo "Building ${env.GREETING} #${env.BUILD_NUMBER} on ${env.BRANCH_NAME}"
        sh 'echo "shell sees $GREETING and $PLAIN"'
        sh "echo interpolated-${env.PLAIN}"
        script {
          env.DYN = 'dynamic'
          def v = sh(script: 'echo captured', returnStdout: true).trim()
          env.CAP = v
          if (params.LOUD) { echo 'LOUD' } else { echo 'quiet' }
        }
        sh 'echo dyn=$DYN cap=$CAP'
        sh 'echo secret-is-$SECRET'
        dir('sub') { sh 'pwd' }
        retry(2) { sh 'echo retried-block' }
      }
    }
    stage('Parallel') {
      parallel {
        stage('A') { steps { sh 'echo branch-a' } }
        stage('B') { steps { sh 'echo branch-b' } }
      }
    }
    stage('Nope') {
      when { branch 'nope' }
      steps { sh 'echo SHOULD_NOT_RUN' }
    }
  }
  post {
    always { echo 'post always' }
    success { sh 'echo post success' }
    failure { echo 'post failure' }
  }
}
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let mut o = RunOptions { provider: "jenkins".into(), file: "Jenkinsfile".into(), repo_path: repo.to_string_lossy().into_owned(), runner: "host".into(), ..Default::default() };
    o.variables = vec![Kv::new("WHO", "tester")];
    let id = ci.start(o, vec![EnvVar { key: "my-secret".into(), value: "s3cr3t-value".into(), secret: true, profile: None }]).unwrap();
    let v = wait(&ci, &id, 180);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "passed", "{d}");
    let b = log_of(&ci, &id, "Build");
    assert!(b.contains("Building Hello tester #1 on main"), "{d}");
    assert!(b.contains("+ echo 'shell sees") || b.contains("+ echo \"shell sees"), "sh -x trace: {d}");
    assert!(b.contains("shell sees Hello tester and x") && b.contains("interpolated-x"), "{d}");
    assert!(b.contains("quiet") && !b.contains("LOUD"), "{d}");
    assert!(b.contains("dyn=dynamic cap=captured"), "{d}");
    assert!(!b.contains("s3cr3t-value") && b.contains("secret-is-***"), "secrets are masked: {d}");
    assert!(b.contains("retried-block"), "{d}");
    assert_eq!(job(&v, "Parallel / A").status, "passed", "{d}");
    assert!(log_of(&ci, &id, "Parallel / B").contains("branch-b"));
    assert_eq!(job(&v, "Nope").status, "skipped");
    let p = log_of(&ci, &id, "post actions");
    assert!(p.contains("post always") && p.contains("post success") && !p.contains("post failure"), "{d}");
}

#[test]
fn jenkins_failure_skips_stages_and_runs_failure_post() {
    let repo = temp_repo("jk-fail");
    write(
        &repo,
        "Jenkinsfile",
        r#"
pipeline {
  agent any
  stages {
    stage('Boom') { steps { sh 'exit 4' } }
    stage('After') { steps { sh 'echo SHOULD_NOT_RUN' } }
  }
  post {
    success { echo 'post success' }
    failure { echo 'post failure ran' }
  }
}
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "jenkins", "Jenkinsfile", |_| {});
    let v = wait(&ci, &id, 120);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "failed", "{d}");
    assert_eq!(job(&v, "After").status, "skipped");
    let p = log_of(&ci, &id, "post actions");
    assert!(p.contains("post failure ran") && !p.contains("post success"), "{d}");
}

// ============================== real runs (Docker) ==============================

fn docker_or_skip() -> bool {
    if !super::exec::docker_ready() {
        eprintln!("docker is not running; skipping");
        return false;
    }
    true
}

#[test]
fn gitlab_job_in_a_container_reaches_its_service_by_alias() {
    if !docker_or_skip() {
        return;
    }
    let repo = temp_repo("gl-docker");
    write(
        &repo,
        ".gitlab-ci.yml",
        r#"
test:
  image: redis:alpine
  services:
    - name: redis:alpine
      alias: cache
  variables:
    FOO: bar
  script:
    - echo "in container $CI_PROJECT_DIR foo=$FOO"
    - cat /etc/os-release | head -n1
    - redis-cli -h cache ping
    - test -f .gitlab-ci.yml
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |o| o.runner = "docker".into());
    let v = wait(&ci, &id, 300);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "passed", "{d}");
    let l = log_of(&ci, &id, "test");
    assert!(l.contains("in container /builds/local/") && l.contains("foo=bar") && l.contains("PONG"), "{d}");
    assert!(job(&v, "test").runner.starts_with("container: redis:alpine"), "{d}");
    // Nothing is left behind.
    let pat = format!("name=^dwci-{}-", id.trim_start_matches("run-"));
    let left = crate::util::run("docker", &["ps", "-aq", "--filter", pat.as_str()], None).unwrap().stdout;
    assert!(left.trim().is_empty() || left.lines().count() == 0, "leftover containers: {left}");
}

#[test]
fn jobs_without_an_image_run_in_a_container_not_on_the_host() {
    if !docker_or_skip() {
        return;
    }
    let repo = temp_repo("gl-default-image");
    write(&repo, ".gitlab-ci.yml", "where:\n  script:\n    - head -n1 /etc/os-release\n    - echo \"cwd=$PWD\"\n");
    write(&repo, "Jenkinsfile", "pipeline {\n  agent any\n  stages {\n    stage('Where') { steps { sh 'head -n1 /etc/os-release' } }\n  }\n}\n");
    git_init(&repo);
    let ci = new_ci();
    let gl = start(&ci, &repo, "gitlab", ".gitlab-ci.yml", |o| {
        o.runner = "auto".into();
        o.ubuntu_image = "redis:alpine".into();
    });
    let jk = start(&ci, &repo, "jenkins", "Jenkinsfile", |o| {
        o.runner = "auto".into();
        o.ubuntu_image = "redis:alpine".into();
    });
    let v = wait(&ci, &gl, 300);
    let l = log_of(&ci, &gl, "where");
    assert_eq!(v.status, "passed", "{}", dump(&ci, &v));
    assert!(l.contains("using the default image `redis:alpine`") && l.contains("Alpine") && l.contains("cwd=/builds/"), "{l}");
    let v = wait(&ci, &jk, 300);
    let l = log_of(&ci, &jk, "Where");
    assert_eq!(v.status, "passed", "{}", dump(&ci, &v));
    assert!(l.contains("Alpine"), "{l}");
}

#[test]
fn github_job_in_a_container_with_a_service_runs_steps_and_env_files() {
    if !docker_or_skip() {
        return;
    }
    let repo = temp_repo("gh-docker");
    write(
        &repo,
        ".github/workflows/c.yml",
        r#"
on: push
jobs:
  t:
    runs-on: ubuntu-latest
    container: redis:alpine
    services:
      cache:
        image: redis:alpine
    steps:
      - uses: actions/checkout@v4
      - id: s
        run: echo "who=$(whoami)" >> "$GITHUB_OUTPUT"
      - run: echo "out=${{ steps.s.outputs.who }} ws=$GITHUB_WORKSPACE"
      - run: redis-cli -h cache ping
"#,
    );
    git_init(&repo);
    let ci = new_ci();
    let id = start(&ci, &repo, "github", ".github/workflows/c.yml", |o| o.runner = "docker".into());
    let v = wait(&ci, &id, 300);
    let d = dump(&ci, &v);
    assert_eq!(v.status, "passed", "{d}");
    let l = log_of(&ci, &id, "t");
    assert!(l.contains("out=root") && l.contains("ws=/home/runner/work/") && l.contains("PONG"), "{d}");
}
