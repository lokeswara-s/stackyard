//! `Jenkinsfile` (declarative, and simple scripted) -> Pipeline.

use std::path::Path;

use super::groovy::{self, Arg, Node, Val};
use super::ir::*;

/// Value prefixes understood by the engine.
pub const GSTR: char = '\u{3}'; // "double quoted" string: interpolate
pub const EXPR: char = '\u{2}'; // Groovy expression: evaluate
pub const CRED: char = '\u{4}'; // credentials('id')

/// Plugin steps that only publish or notify; they are skipped (with a note) because they need a Jenkins server.
const SKIPPED: &[&str] = &[
    "checkout", "cleanWs", "deleteDir", "publishHTML", "publishChecks", "recordIssues", "slackSend", "mail", "emailext", "cobertura", "jacoco", "publishCoverage", "junit_",
    "archive", "fingerprint", "milestone", "githubNotify", "setGitHubPullRequestStatus", "discordSend", "office365ConnectorSend", "step", "properties", "buildDescription",
    "echo_", "stage_", "timestamps", "ansiColor", "waitUntil_",
];

/// Wrapper steps that just run their body.
const TRANSPARENT: &[&str] = &["script", "node", "ansiColor", "timestamps", "lock", "wrap", "withSonarQubeEnv", "withMaven", "withGradle", "steps", "container", "stage", "ws", "withAnsiColor", "tool"];

fn text_of(v: &Val) -> String {
    match v {
        Val::Str { v, interp: true } => format!("{GSTR}{v}"),
        Val::Str { v, interp: false } => v.clone(),
        Val::Num(n) => n.clone(),
        Val::Expr(e) => {
            let t = e.trim();
            if let Some(rest) = t.strip_prefix("credentials(") {
                let inner = rest.trim_end_matches(')').trim().trim_matches(|c| c == '\'' || c == '"');
                return format!("{CRED}{inner}");
            }
            format!("{EXPR}{t}")
        }
        Val::List(_) | Val::Block(_) => String::new(),
    }
}

fn plain(v: &Val) -> Option<String> {
    match v {
        Val::Str { v, .. } => Some(v.clone()),
        Val::Num(n) => Some(n.clone()),
        Val::Expr(e) => Some(e.trim().to_string()),
        _ => None,
    }
}

fn num_of(v: &Val) -> Option<f64> {
    plain(v).and_then(|s| s.parse().ok())
}

fn unit_secs(unit: &str) -> f64 {
    match unit.to_uppercase().as_str() {
        "NANOSECONDS" | "MICROSECONDS" | "MILLISECONDS" => 0.001,
        "SECONDS" => 1.0,
        "MINUTES" => 60.0,
        "HOURS" => 3600.0,
        "DAYS" => 86400.0,
        _ => 60.0,
    }
}

fn time_arg(n: &Node, default_unit: &str) -> Option<u64> {
    let t = n.arg("time").or_else(|| n.args.iter().find(|a| a.name.is_none()).map(|a| &a.val)).and_then(num_of)?;
    let unit = n.arg("unit").and_then(plain).unwrap_or_else(|| default_unit.into());
    Some((t * unit_secs(&unit)).ceil() as u64)
}

fn list_items(v: Option<&Val>) -> Vec<&Arg> {
    match v {
        Some(Val::List(items)) => items.iter().collect(),
        _ => vec![],
    }
}

/// Parse an expression such as `string(credentialsId: 'x', variable: 'V')` that was captured as source text.
fn inner_calls(a: &Arg) -> Vec<Node> {
    match &a.val {
        Val::Expr(e) => groovy::parse(e),
        _ => vec![],
    }
}

#[derive(Default, Clone)]
struct Agent {
    image: Option<String>,
    args: Vec<String>,
    none: bool,
}

fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for ch in s.chars() {
        match (q, ch) {
            (Some(c), x) if x == c => q = None,
            (Some(_), x) => cur.push(x),
            (None, '"') | (None, '\'') => q = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_agent(n: &Node, warnings: &mut Vec<String>) -> Agent {
    let mut a = Agent::default();
    if let Some(b) = &n.block {
        for c in b {
            match c.name.as_str() {
                "docker" => {
                    if let Some(blk) = &c.block {
                        for d in blk {
                            match d.name.as_str() {
                                "image" => a.image = d.first("image").and_then(plain),
                                "args" => a.args = d.first("args").and_then(plain).map(|s| split_args(&s)).unwrap_or_default(),
                                _ => {}
                            }
                        }
                    } else {
                        a.image = c.first("image").and_then(plain);
                    }
                }
                "dockerfile" => warnings.push("`agent { dockerfile }` is not supported; the stage runs on this machine".into()),
                _ => {}
            }
        }
    } else if n.args.first().and_then(|x| plain(&x.val)).as_deref() == Some("none") {
        a.none = true;
    }
    a
}

/// `when { ... }` -> a Groovy expression using `__branch(...)`-style predicates.
fn when_expr(nodes: &[Node]) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
    let parts: Vec<String> = nodes
        .iter()
        .map(|n| match n.name.as_str() {
            "branch" => format!("__branch({})", q(&n.first("pattern").and_then(plain).unwrap_or_default())),
            "tag" => format!("__tag({})", q(&n.first("pattern").and_then(plain).unwrap_or_default())),
            "buildingTag" => "__buildingTag()".into(),
            "changeRequest" => "__changeRequest()".into(),
            "changeset" => format!("__changeset({})", q(&n.first("pattern").and_then(plain).unwrap_or_default())),
            "triggeredBy" => format!("__triggeredBy({})", q(&n.first("cause").and_then(plain).unwrap_or_default())),
            "environment" => format!("(env.{} == {})", n.arg("name").and_then(plain).unwrap_or_default(), q(&n.arg("value").and_then(plain).unwrap_or_default())),
            "equals" => format!("(({}) == ({}))", n.arg("actual").and_then(plain).unwrap_or_default(), n.arg("expected").and_then(plain).unwrap_or_default()),
            "expression" => n.block.as_ref().map(|b| format!("({})", b.iter().map(|x| x.raw.trim().to_string()).collect::<Vec<_>>().join(" && "))).unwrap_or_else(|| "true".into()),
            "not" => format!("!({})", when_expr(n.block.as_deref().unwrap_or(&[]))),
            "allOf" => format!("({})", when_expr(n.block.as_deref().unwrap_or(&[]))),
            "anyOf" => {
                let inner: Vec<String> = n.block.as_deref().unwrap_or(&[]).iter().map(|c| when_expr(std::slice::from_ref(c))).collect();
                format!("({})", inner.join(" || "))
            }
            _ => "true".into(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        "true".into()
    } else {
        parts.join(" && ")
    }
}

fn first_line(s: &str) -> String {
    let t = s.trim().lines().next().unwrap_or("").trim().to_string();
    if t.chars().count() > 80 {
        format!("{}...", t.chars().take(77).collect::<String>())
    } else {
        t
    }
}

fn script_step(n: &Node, shell: &str, label_default: &str) -> Step {
    let val = n.first("script");
    let (text, interp) = match val {
        Some(Val::Str { v, interp }) => (v.clone(), *interp),
        Some(Val::Num(x)) => (x.clone(), false),
        Some(Val::Expr(e)) => (format!("{EXPR}{}", e.trim()), false),
        _ => (String::new(), false),
    };
    let label = n.arg("label").and_then(plain);
    let preview = if text.starts_with(EXPR) { "(expression)".to_string() } else { first_line(&text) };
    let name = label.unwrap_or_else(|| format!("{label_default}: {preview}"));
    Step::new(name, StepKind::Script { script: text, style: shell.into(), interpolate: interp })
}

fn apply_all(steps: &mut [Step], f: &dyn Fn(&mut Step)) {
    for s in steps.iter_mut() {
        f(s);
        match &mut s.kind {
            StepKind::If { then, otherwise, .. } => {
                apply_all(then, f);
                apply_all(otherwise, f);
            }
            StepKind::Try { body, catch_, finally } => {
                apply_all(body, f);
                apply_all(catch_, f);
                apply_all(finally, f);
            }
            StepKind::Parallel { branches } => {
                for (_, b) in branches.iter_mut() {
                    apply_all(b, f);
                }
            }
            _ => {}
        }
    }
}

fn assignment(lhs: &str, rhs: &Val) -> Option<Step> {
    let (key, env) = if let Some(k) = lhs.strip_prefix("env.") { (k.to_string(), true) } else { (lhs.to_string(), false) };
    // env.X = sh(script: '...', returnStdout: true).trim()
    let capture = match rhs {
        Val::Expr(e) if e.trim_start().starts_with("sh") && e.contains("returnStdout") => {
            let calls = groovy::parse(e);
            calls.first().and_then(|c| c.first("script")).map(text_of)
        }
        _ => None,
    };
    let expr = match rhs {
        Val::Str { v, interp: true } => format!("{GSTR}{v}"),
        Val::Str { v, interp: false } => v.clone(),
        Val::Num(n) => format!("{EXPR}{n}"),
        Val::Expr(e) => format!("{EXPR}{}", e.trim()),
        _ => return None,
    };
    let mut st = Step::new(format!("set {lhs}"), StepKind::SetVar { key, expr, script: capture, env });
    st.name = format!("set {lhs}");
    Some(st)
}

fn steps_from(nodes: &[Node], warnings: &mut Vec<String>) -> Vec<Step> {
    let mut out = vec![];
    let mut i = 0;
    while i < nodes.len() {
        let n = &nodes[i];
        i += 1;
        match n.name.as_str() {
            "sh" => out.push(script_step(n, "jenkins", "sh")),
            "bat" => out.push(script_step(n, "bat", "bat")),
            "powershell" | "pwsh" => out.push(script_step(n, "pwsh", &n.name)),
            "echo" => {
                let (text, interp) = match n.first("message") {
                    Some(Val::Str { v, interp }) => (v.clone(), *interp),
                    Some(Val::Expr(e)) => (format!("{EXPR}{}", e.trim()), false),
                    Some(Val::Num(x)) => (x.clone(), false),
                    _ => (String::new(), false),
                };
                out.push(Step::new(format!("echo: {}", first_line(&text)), StepKind::Echo { text, interpolate: interp }));
            }
            "error" => {
                let msg = n.first("message").map(text_of).unwrap_or_default();
                out.push(Step::new("error", StepKind::Error { message: msg }));
            }
            "unstable" => out.push(Step::new("unstable", StepKind::SetVar { key: "currentBuild.result".into(), expr: "UNSTABLE".into(), script: None, env: false })),
            "sleep" => {
                let secs = time_arg(n, "SECONDS").unwrap_or(1);
                out.push(Step::new(format!("sleep {secs}s"), StepKind::Sleep { seconds: secs }));
            }
            "archiveArtifacts" => {
                let patterns = n.first("artifacts").and_then(plain).unwrap_or_default();
                let allow = n.arg("allowEmptyArchive").and_then(plain).map(|v| v == "true").unwrap_or(false);
                out.push(Step::new(format!("archiveArtifacts {patterns}"), StepKind::Archive { patterns, allow_empty: allow }));
            }
            "stash" => {
                let name = n.first("name").and_then(plain).unwrap_or_default();
                let includes = n.arg("includes").and_then(plain).unwrap_or_else(|| "**".into());
                out.push(Step::new(format!("stash {name}"), StepKind::Stash { name, includes }));
            }
            "unstash" => {
                let name = n.first("name").and_then(plain).unwrap_or_default();
                out.push(Step::new(format!("unstash {name}"), StepKind::Unstash { name }));
            }
            "junit" => {
                let pattern = n.first("testResults").and_then(plain).unwrap_or_default();
                out.push(Step::new(format!("junit {pattern}"), StepKind::Junit { pattern }));
            }
            "input" => {
                let message = n.first("message").and_then(plain).unwrap_or_else(|| "Proceed?".into());
                out.push(Step::new("input", StepKind::Input { message }));
            }
            "dir" => {
                let path = n.first("path").map(text_of).unwrap_or_default();
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| {
                    s.dir = Some(match &s.dir {
                        Some(d) => format!("{path}/{d}"),
                        None => path.clone(),
                    })
                });
                out.extend(inner);
            }
            "withEnv" => {
                let mut kvs = vec![];
                for a in list_items(n.first("env")) {
                    let t = text_of(&a.val);
                    let raw = t.trim_start_matches(GSTR).to_string();
                    if let Some((k, v)) = raw.split_once('=') {
                        kvs.push(Kv::new(k.trim(), if t.starts_with(GSTR) { format!("{GSTR}{v}") } else { v.to_string() }));
                    }
                }
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| {
                    for kv in &kvs {
                        if !s.env.iter().any(|e| e.key == kv.key) {
                            s.env.push(kv.clone());
                        }
                    }
                });
                out.extend(inner);
            }
            "withCredentials" => {
                let mut kvs = vec![];
                for a in list_items(n.first("bindings")) {
                    for c in inner_calls(a) {
                        let id = c.arg("credentialsId").and_then(plain).unwrap_or_default();
                        match c.name.as_str() {
                            "usernamePassword" => {
                                if let Some(u) = c.arg("usernameVariable").and_then(plain) {
                                    kvs.push(Kv::new(u, format!("{CRED}{id}:USR")));
                                }
                                if let Some(p) = c.arg("passwordVariable").and_then(plain) {
                                    kvs.push(Kv::new(p, format!("{CRED}{id}:PSW")));
                                }
                            }
                            _ => {
                                if let Some(v) = c.arg("variable").and_then(plain) {
                                    kvs.push(Kv::new(v, format!("{CRED}{id}")));
                                }
                            }
                        }
                    }
                }
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| {
                    for kv in &kvs {
                        if !s.env.iter().any(|e| e.key == kv.key) {
                            s.env.push(kv.clone());
                        }
                    }
                });
                out.extend(inner);
            }
            "timeout" => {
                let secs = time_arg(n, "MINUTES");
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| {
                    if s.timeout_secs.is_none() {
                        s.timeout_secs = secs;
                    }
                });
                out.extend(inner);
            }
            "retry" => {
                let count = n.args.iter().find(|a| a.name.is_none() || a.name.as_deref() == Some("count")).and_then(|a| num_of(&a.val)).unwrap_or(1.0) as u32;
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| {
                    if s.retry == 0 {
                        s.retry = count.saturating_sub(1);
                    }
                });
                out.extend(inner);
            }
            "catchError" | "warnError" | "unsuccessful" => {
                let mut inner = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                apply_all(&mut inner, &|s| s.catch_error = true);
                out.extend(inner);
            }
            "withCredentials_" => {}
            "parallel" => {
                let mut branches = vec![];
                for a in &n.args {
                    if let (Some(name), Val::Block(b)) = (&a.name, &a.val) {
                        branches.push((name.clone(), steps_from(b, warnings)));
                    }
                }
                if !branches.is_empty() {
                    out.push(Step::new("parallel", StepKind::Parallel { branches }));
                }
            }
            "if" | "else if" => {
                let cond = n.args.first().and_then(|a| plain(&a.val)).unwrap_or_default();
                let then = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                // Gather the else chain.
                let mut otherwise = vec![];
                if let Some(next) = nodes.get(i) {
                    if next.name == "else" {
                        otherwise = steps_from(next.block.as_deref().unwrap_or(&[]), warnings);
                        i += 1;
                    } else if next.name == "else if" {
                        // Rewrite as `else { if ... }` by recursing on the remaining chain.
                        let rest: Vec<Node> = nodes[i..].iter().cloned().collect();
                        let mut r = rest;
                        r[0].name = "if".into();
                        let consumed = chain_len(&r);
                        otherwise = steps_from(&r[..consumed], warnings);
                        i += consumed;
                    }
                }
                out.push(Step::new(format!("if ({cond})"), StepKind::If { cond, then, otherwise }));
            }
            "try" => {
                let body = steps_from(n.block.as_deref().unwrap_or(&[]), warnings);
                let mut catch_ = vec![];
                let mut finally = vec![];
                while let Some(next) = nodes.get(i) {
                    match next.name.as_str() {
                        "catch" => catch_ = steps_from(next.block.as_deref().unwrap_or(&[]), warnings),
                        "finally" => finally = steps_from(next.block.as_deref().unwrap_or(&[]), warnings),
                        _ => break,
                    }
                    i += 1;
                }
                out.push(Step::new("try", StepKind::Try { body, catch_, finally }));
            }
            "=" => {
                let lhs = match n.args.first().map(|a| &a.val) {
                    Some(Val::Str { v, .. }) => v.clone(),
                    _ => String::new(),
                };
                match n.args.get(1).and_then(|a| assignment(&lhs, &a.val)) {
                    Some(s) => out.push(s),
                    None => out.push(Step::new(format!("assignment {lhs}"), StepKind::Unsupported { what: n.raw.clone() })),
                }
            }
            "def" | "String" | "int" | "boolean" | "def_" => {
                // `def x = expr` is captured as one expression "x = expr"
                let raw = n.args.first().and_then(|a| plain(&a.val)).unwrap_or_default();
                if let Some((l, r)) = raw.split_once('=') {
                    let rv = Parser2::value(r.trim());
                    if let Some(s) = assignment(l.trim(), &rv) {
                        out.push(s);
                        continue;
                    }
                }
                out.push(Step::new("def", StepKind::Unsupported { what: n.raw.clone() }));
            }
            name if TRANSPARENT.contains(&name) && n.block.is_some() => {
                // `script { }`, `node('x') { }`, ...: run the body.
                out.extend(steps_from(n.block.as_deref().unwrap_or(&[]), warnings));
            }
            name if SKIPPED.contains(&name) => {
                out.push(Step::new(
                    format!("{name} (skipped)"),
                    StepKind::Echo { text: format!("`{name}` needs a Jenkins server or plugin; skipped by the simulator"), interpolate: false },
                ));
            }
            "?" | "return" | "break" | "continue" | "for" | "while" | "switch" | "throw" => {
                warnings.push(format!("Groovy `{}` cannot be evaluated by the simulator", first_line(&n.raw)));
                out.push(Step::new(format!("groovy: {}", first_line(&n.raw)), StepKind::Unsupported { what: n.raw.clone() }));
            }
            other => {
                warnings.push(format!("step `{other}` is not supported by the simulator"));
                out.push(Step::new(format!("{other}"), StepKind::Unsupported { what: n.raw.clone() }));
            }
        }
    }
    out
}

/// How many nodes the `if ... else if ... else` chain at the start of `nodes` spans.
fn chain_len(nodes: &[Node]) -> usize {
    let mut n = 1;
    while let Some(x) = nodes.get(n) {
        if x.name == "else if" {
            n += 1;
        } else if x.name == "else" {
            return n + 1;
        } else {
            break;
        }
    }
    n
}

/// Turn a source fragment (`'a'`, `"b ${x}"`, `foo + 1`) into a `Val`.
struct Parser2;
impl Parser2 {
    fn value(src: &str) -> Val {
        let toks = groovy::lex(src);
        match toks.as_slice() {
            [t] if t.kind == groovy::TK::Str => Val::Str { v: t.text.clone(), interp: t.interp },
            [t] if t.kind == groovy::TK::Num => Val::Num(t.text.clone()),
            _ => Val::Expr(src.to_string()),
        }
    }
}

fn parse_post(n: &Node, warnings: &mut Vec<String>) -> Vec<PostBlock> {
    let mut out = vec![];
    for b in n.block.as_deref().unwrap_or(&[]) {
        out.push(PostBlock { cond: b.name.clone(), steps: steps_from(b.block.as_deref().unwrap_or(&[]), warnings) });
    }
    out
}

fn env_of(n: &Node) -> Vec<Kv> {
    let mut out = vec![];
    for e in n.block.as_deref().unwrap_or(&[]) {
        if e.name == "=" {
            if let (Some(Val::Str { v: k, .. }), Some(val)) = (e.args.first().map(|a| &a.val), e.args.get(1).map(|a| &a.val)) {
                put(&mut out, k, text_of(val));
            }
        }
    }
    out
}

struct B<'a> {
    p: &'a mut Pipeline,
    agent: Agent,
    timeout: Option<u64>,
}

impl<'a> B<'a> {
    fn stage(&mut self, n: &Node, parent: Option<&str>, parent_agent: &Agent, inherited_env: &[Kv]) {
        let title = n.first("name").and_then(plain).unwrap_or_else(|| "stage".into());
        let mut agent = parent_agent.clone();
        let mut env: Vec<Kv> = inherited_env.to_vec();
        let mut cond: Option<String> = None;
        let mut steps: Vec<Step> = vec![];
        let mut post: Vec<PostBlock> = vec![];
        let mut children: Vec<&Node> = vec![];
        let mut parallel: Vec<&Node> = vec![];
        let mut timeout = self.timeout;
        let mut retry = 0;
        for c in n.block.as_deref().unwrap_or(&[]) {
            match c.name.as_str() {
                "agent" => {
                    let a = parse_agent(c, &mut self.p.warnings);
                    agent = a;
                }
                "environment" => {
                    for kv in env_of(c) {
                        put(&mut env, &kv.key, kv.value);
                    }
                }
                "when" => cond = Some(when_expr(c.block.as_deref().unwrap_or(&[]))),
                "steps" => steps = steps_from(c.block.as_deref().unwrap_or(&[]), &mut self.p.warnings),
                "post" => post = parse_post(c, &mut self.p.warnings),
                "stages" => children = c.block.as_deref().unwrap_or(&[]).iter().filter(|x| x.name == "stage").collect(),
                "parallel" => parallel = c.block.as_deref().unwrap_or(&[]).iter().filter(|x| x.name == "stage").collect(),
                "options" => {
                    for o in c.block.as_deref().unwrap_or(&[]) {
                        match o.name.as_str() {
                            "timeout" => timeout = time_arg(o, "MINUTES"),
                            "retry" => retry = o.args.first().and_then(|a| num_of(&a.val)).unwrap_or(1.0) as u32,
                            _ => {}
                        }
                    }
                }
                "input" => {
                    let message = c.first("message").and_then(plain).unwrap_or_else(|| "Proceed?".into());
                    steps.insert(0, Step::new("input", StepKind::Input { message }));
                }
                _ => {}
            }
        }
        let host = agent.image.is_none();
        let stage_name = match parent {
            Some(p) if !parallel.is_empty() || !children.is_empty() => p.to_string(),
            Some(p) => p.to_string(),
            None => title.clone(),
        };
        let mk = |id: String, stage: String, steps: Vec<Step>| -> Job {
            let mut j = Job::new(id.clone(), stage);
            j.name = id;
            j.image = agent.image.clone();
            j.docker_args = agent.args.clone();
            j.env = env.clone();
            j.steps = steps;
            j.cond = cond.clone();
            j.shared_workspace = true;
            j.timeout_secs = timeout;
            j.retry = retry;
            j.post = post.clone();
            let _ = host;
            j
        };

        if !parallel.is_empty() {
            if !self.p.stages.contains(&stage_name) {
                self.p.stages.push(stage_name.clone());
            }
            for pn in parallel {
                let child_title = pn.first("name").and_then(plain).unwrap_or_else(|| "branch".into());
                let id = format!("{title} / {child_title}");
                let before = self.p.jobs.len();
                self.stage(pn, Some(&stage_name), &agent, &env);
                // Prefix the id so that parallel branches are unambiguous.
                if let Some(j) = self.p.jobs.get_mut(before) {
                    j.id = id.clone();
                    j.name = id;
                }
                // A parent `when` applies to the branches.
                if let (Some(c), Some(j)) = (&cond, self.p.jobs.get_mut(before)) {
                    j.cond = Some(match &j.cond {
                        Some(x) => format!("({c}) && ({x})"),
                        None => c.clone(),
                    });
                }
            }
            return;
        }
        if !children.is_empty() {
            for (k, cn) in children.iter().enumerate() {
                let sub_title = cn.first("name").and_then(plain).unwrap_or_else(|| format!("{k}"));
                let sname = format!("{title} \u{203A} {sub_title}");
                self.p.stages.push(sname.clone());
                let before = self.p.jobs.len();
                self.stage(cn, Some(&sname), &agent, &env);
                if let (Some(c), Some(j)) = (&cond, self.p.jobs.get_mut(before)) {
                    j.cond = Some(match &j.cond {
                        Some(x) => format!("({c}) && ({x})"),
                        None => c.clone(),
                    });
                }
                if let Some(j) = self.p.jobs.get_mut(before) {
                    let id = format!("{title} / {sub_title}");
                    j.id = id.clone();
                    j.name = id;
                }
            }
            return;
        }
        let top = parent.is_none();
        if top && !self.p.stages.contains(&stage_name) {
            self.p.stages.push(stage_name.clone());
        }
        let stage_for_job = if top { stage_name } else { parent.unwrap_or("").to_string() };
        let id = title.clone();
        self.p.jobs.push(mk(id, stage_for_job, steps));
    }
}

pub fn parse(repo: &Path, file: &str) -> Result<Pipeline, String> {
    let path = repo.join(file);
    let src = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let nodes = groovy::parse(&src);
    let mut p = Pipeline { provider: "jenkins".into(), file: file.into(), name: "Jenkinsfile".into(), gated_by_stage: true, host_agent: true, ..Default::default() };

    let decl = nodes.iter().find(|n| n.name == "pipeline" && n.block.is_some());
    let Some(decl) = decl else {
        return parse_scripted(p, &nodes);
    };
    let mut agent = Agent::default();
    let mut stage_nodes: Vec<&Node> = vec![];
    let mut timeout = None;
    for n in decl.block.as_deref().unwrap_or(&[]) {
        match n.name.as_str() {
            "agent" => agent = parse_agent(n, &mut p.warnings),
            "environment" => p.env = env_of(n),
            "options" => {
                for o in n.block.as_deref().unwrap_or(&[]) {
                    if o.name == "timeout" {
                        timeout = time_arg(o, "MINUTES");
                    }
                }
            }
            "parameters" => {
                for pr in n.block.as_deref().unwrap_or(&[]) {
                    let name = pr.arg("name").and_then(plain).unwrap_or_default();
                    let desc = pr.arg("description").and_then(plain).unwrap_or_default();
                    let (default, kind, choices) = match pr.name.as_str() {
                        "booleanParam" => (pr.arg("defaultValue").and_then(plain).unwrap_or_else(|| "false".into()), "boolean", vec![]),
                        "choice" => {
                            let items: Vec<String> = match pr.arg("choices") {
                                Some(Val::List(l)) => l.iter().filter_map(|a| plain(&a.val)).collect(),
                                Some(Val::Str { v, .. }) => v.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                                _ => vec![],
                            };
                            (items.first().cloned().unwrap_or_default(), "choice", items)
                        }
                        _ => (pr.arg("defaultValue").and_then(plain).unwrap_or_default(), "string", vec![]),
                    };
                    p.params.push(Param { name, default, description: desc, kind: kind.into(), choices });
                }
            }
            "stages" => stage_nodes = n.block.as_deref().unwrap_or(&[]).iter().filter(|x| x.name == "stage").collect(),
            "post" => {
                let blocks = parse_post(n, &mut p.warnings);
                p.post = blocks;
            }
            "tools" => p.warnings.push("`tools { }` selects Jenkins-managed tool installs; the simulator uses what is in the container/on this machine".into()),
            _ => {}
        }
    }
    if stage_nodes.is_empty() {
        return Err("no `stages { stage(...) { ... } }` found in the pipeline".into());
    }
    p.default_image = agent.image.clone();
    p.default_docker_args = agent.args.clone();
    p.host_agent = agent.image.is_none();
    {
        let mut b = B { p: &mut p, agent: agent.clone(), timeout };
        let envs: Vec<Kv> = vec![];
        for sn in stage_nodes {
            let ag = b.agent.clone();
            b.stage(sn, None, &ag, &envs);
        }
    }
    if !p.post.is_empty() {
        let mut j = Job::new("post actions", "post");
        j.when = "always".into();
        j.post = p.post.clone();
        j.shared_workspace = true;
        j.image = agent.image.clone();
        j.docker_args = agent.args.clone();
        p.stages.push("post".into());
        p.jobs.push(j);
    }
    // Jobs without their own agent run where the pipeline runs.
    Ok(p)
}

fn parse_scripted(mut p: Pipeline, nodes: &[Node]) -> Result<Pipeline, String> {
    p.warnings.push("This is a scripted Jenkinsfile. Only simple `node { stage('x') { steps } }` structures are understood.".into());
    // Flatten node { ... } wrappers.
    fn flat<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
        for n in nodes {
            if (n.name == "node" || n.name == "timestamps" || n.name == "ansiColor") && n.block.is_some() {
                flat(n.block.as_deref().unwrap(), out);
            } else {
                out.push(n);
            }
        }
    }
    let mut top = vec![];
    flat(nodes, &mut top);
    let mut stage_jobs = vec![];
    let mut loose: Vec<Node> = vec![];
    for n in top {
        if n.name == "stage" && n.block.is_some() {
            stage_jobs.push(n);
        } else {
            loose.push(n.clone());
        }
    }
    let mut warnings = std::mem::take(&mut p.warnings);
    if stage_jobs.is_empty() {
        let steps = steps_from(&loose, &mut warnings);
        if steps.is_empty() {
            return Err("no steps found in this Jenkinsfile".into());
        }
        let mut j = Job::new("Build", "Build");
        j.steps = steps;
        j.shared_workspace = true;
        p.stages.push("Build".into());
        p.jobs.push(j);
    } else {
        for s in stage_jobs {
            let name = s.first("name").and_then(plain).unwrap_or_else(|| "stage".into());
            let mut j = Job::new(name.clone(), name.clone());
            j.steps = steps_from(s.block.as_deref().unwrap_or(&[]), &mut warnings);
            j.shared_workspace = true;
            p.stages.push(name);
            p.jobs.push(j);
        }
    }
    p.warnings = warnings;
    Ok(p)
}
