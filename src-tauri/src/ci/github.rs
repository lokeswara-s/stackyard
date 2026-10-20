//! `.github/workflows/*.yml` -> Pipeline.

use serde_json::Value as J;
use serde_yaml::Value as Y;
use std::path::Path;

use super::gitlab::{list_of_str, s};
use super::ir::*;

fn to_json(v: &Y) -> J {
    serde_json::to_value(v).unwrap_or(J::Null)
}

fn kv_map(v: Option<&Y>) -> Vec<Kv> {
    let mut out = vec![];
    if let Some(Y::Mapping(m)) = v {
        for (k, val) in m {
            if let Some(k) = s(k) {
                put(&mut out, &k, s(val).unwrap_or_default());
            }
        }
    }
    out
}

/// Seconds from `timeout-minutes` (a number, or an expression we cannot resolve yet).
fn minutes(v: Option<&Y>) -> Option<u64> {
    v.and_then(|x| x.as_f64()).map(|m| (m * 60.0) as u64)
}

fn split_opts(o: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for ch in o.chars() {
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

pub fn parse_step(st: &Y, idx: usize, warnings: &mut Vec<String>, job: &str) -> Step {
    let name = st.get("name").and_then(s).or_else(|| st.get("uses").and_then(s).map(|u| format!("Run {u}"))).or_else(|| st.get("run").and_then(s).map(|r| format!("Run {}", r.lines().next().unwrap_or("").trim()))).unwrap_or_else(|| format!("Step {}", idx + 1));
    let kind = if let Some(u) = st.get("uses").and_then(s) {
        let with = kv_map(st.get("with"));
        StepKind::Uses { action: u, with }
    } else if let Some(r) = st.get("run").and_then(s) {
        StepKind::Script { script: r, style: "github".into(), interpolate: false }
    } else {
        warnings.push(format!("{job}: step {} has neither `run` nor `uses`", idx + 1));
        StepKind::Unsupported { what: "empty step".into() }
    };
    let mut step = Step::new(name, kind);
    step.id = st.get("id").and_then(s);
    step.env = kv_map(st.get("env"));
    step.cond = st.get("if").and_then(s);
    step.continue_on_error = st.get("continue-on-error").and_then(|v| v.as_bool()).unwrap_or(false);
    step.dir = st.get("working-directory").and_then(s);
    step.shell = st.get("shell").and_then(s);
    step.timeout_secs = minutes(st.get("timeout-minutes"));
    step
}

fn dims(m: &serde_yaml::Mapping) -> Vec<(String, Vec<J>)> {
    let mut out = vec![];
    for (k, v) in m {
        let key = s(k).unwrap_or_default();
        if key == "include" || key == "exclude" {
            continue;
        }
        let vals: Vec<J> = match v {
            Y::Sequence(a) => a.iter().map(to_json).collect(),
            other => vec![to_json(other)],
        };
        out.push((key, vals));
    }
    out
}

fn expand_matrix(m: &serde_yaml::Mapping) -> Vec<Vec<(String, J)>> {
    let d = dims(m);
    let mut combos: Vec<Vec<(String, J)>> = if d.is_empty() { vec![] } else { vec![vec![]] };
    for (k, vals) in &d {
        let mut next = vec![];
        for base in &combos {
            for v in vals {
                let mut b = base.clone();
                b.push((k.clone(), v.clone()));
                next.push(b);
            }
        }
        combos = next;
    }
    let obj = |y: &Y| -> Vec<(String, J)> {
        match y {
            Y::Mapping(mm) => mm.iter().filter_map(|(k, v)| s(k).map(|k| (k, to_json(v)))).collect(),
            _ => vec![],
        }
    };
    if let Some(Y::Sequence(ex)) = m.get("exclude") {
        for e in ex {
            let e = obj(e);
            combos.retain(|c| !e.iter().all(|(k, v)| c.iter().any(|(ck, cv)| ck == k && cv == v)));
        }
    }
    if let Some(Y::Sequence(inc)) = m.get("include") {
        let orig: Vec<String> = d.iter().map(|(k, _)| k.clone()).collect();
        for i in inc {
            let i = obj(i);
            let mut matched = false;
            for c in combos.iter_mut() {
                let fits = i.iter().filter(|(k, _)| orig.contains(k)).all(|(k, v)| c.iter().any(|(ck, cv)| ck == k && cv == v));
                if fits {
                    matched = true;
                    for (k, v) in &i {
                        if orig.contains(k) {
                            continue;
                        }
                        if let Some(e) = c.iter_mut().find(|(ck, _)| ck == k) {
                            e.1 = v.clone();
                        } else {
                            c.push((k.clone(), v.clone()));
                        }
                    }
                }
            }
            if !matched {
                combos.push(i);
            }
        }
    }
    combos
}

fn label(v: &J) -> String {
    match v {
        J::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn parse(repo: &Path, file: &str) -> Result<Pipeline, String> {
    let path = repo.join(file);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut doc: Y = serde_yaml::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
    doc.apply_merge().map_err(|e| format!("{file}: {e}"))?;
    let mut p = Pipeline { provider: "github".into(), file: file.into(), ..Default::default() };
    p.name = doc.get("name").and_then(s).unwrap_or_else(|| file.rsplit('/').next().unwrap_or(file).to_string());
    p.env = kv_map(doc.get("env"));
    let run_def = doc.get("defaults").and_then(|d| d.get("run"));
    p.default_shell = run_def.and_then(|r| r.get("shell")).and_then(s);
    p.default_dir = run_def.and_then(|r| r.get("working-directory")).and_then(s);

    let on = doc.get("on").or_else(|| doc.as_mapping().and_then(|m| m.get(&Y::Bool(true))));
    match on {
        Some(Y::String(e)) => p.events.push(e.clone()),
        Some(Y::Sequence(a)) => p.events = a.iter().filter_map(s).collect(),
        Some(Y::Mapping(m)) => {
            for (k, v) in m {
                if let Some(k) = s(k) {
                    if k == "workflow_dispatch" {
                        if let Some(Y::Mapping(inputs)) = v.get("inputs") {
                            for (n, def) in inputs {
                                p.params.push(Param {
                                    name: s(n).unwrap_or_default(),
                                    default: def.get("default").and_then(s).unwrap_or_default(),
                                    description: def.get("description").and_then(s).unwrap_or_default(),
                                    kind: def.get("type").and_then(s).unwrap_or_else(|| "string".into()),
                                    choices: list_of_str(def.get("options")),
                                });
                            }
                        }
                    }
                    p.events.push(k);
                }
            }
        }
        _ => {}
    }

    let Some(Y::Mapping(jobs)) = doc.get("jobs") else { return Err("no `jobs:` in this workflow".into()) };
    for (jk, j) in jobs {
        let id = s(jk).unwrap_or_default();
        let base_name = j.get("name").and_then(s).unwrap_or_else(|| id.clone());
        let mut base = Job::new(id.clone(), "job");
        base.name = base_name.clone();
        base.has_needs = true;
        base.needs = list_of_str(j.get("needs")).into_iter().map(|job| Need { job, artifacts: true, optional: false }).collect();
        base.cond = j.get("if").and_then(s);
        base.runs_on = match j.get("runs-on") {
            Some(Y::Sequence(a)) => a.iter().filter_map(s).next(),
            Some(Y::Mapping(m)) => m.get("labels").map(|l| list_of_str(Some(l))).and_then(|l| l.into_iter().next()).or_else(|| m.get("group").and_then(s)),
            Some(v) => s(v),
            None => None,
        };
        base.env = kv_map(j.get("env"));
        base.allow_failure = j.get("continue-on-error").and_then(|v| v.as_bool()).unwrap_or(false);
        base.timeout_secs = Some(minutes(j.get("timeout-minutes")).unwrap_or(360 * 60));
        base.outputs = kv_map(j.get("outputs"));
        let rd = j.get("defaults").and_then(|d| d.get("run"));
        base.default_shell = rd.and_then(|r| r.get("shell")).and_then(s).or_else(|| p.default_shell.clone());
        base.default_dir = rd.and_then(|r| r.get("working-directory")).and_then(s).or_else(|| p.default_dir.clone());

        match j.get("container") {
            Some(Y::Mapping(_)) => {
                let c = j.get("container").unwrap();
                base.image = c.get("image").and_then(s);
                base.docker_args = c.get("options").and_then(s).map(|o| split_opts(&o)).unwrap_or_default();
                for kv in kv_map(c.get("env")) {
                    put(&mut base.env, &kv.key, kv.value);
                }
            }
            Some(v) => base.image = s(v),
            None => {}
        }
        if let Some(Y::Mapping(svcs)) = j.get("services") {
            for (alias, sv) in svcs {
                let alias = s(alias).unwrap_or_default();
                let (image, env, ports, options) = match sv {
                    Y::Mapping(_) => (sv.get("image").and_then(s).unwrap_or_default(), kv_map(sv.get("env")), list_of_str(sv.get("ports")), sv.get("options").and_then(s).map(|o| split_opts(&o)).unwrap_or_default()),
                    other => (s(other).unwrap_or_default(), vec![], vec![], vec![]),
                };
                base.services.push(ServiceDef { image, alias, env, command: None, options, ports });
            }
        }

        if let Some(u) = j.get("uses").and_then(s) {
            p.warnings.push(format!("job `{id}` calls the reusable workflow `{u}`; reusable workflows are not supported yet"));
            base.steps.push(Step::new(format!("Reusable workflow {u}"), StepKind::Unsupported { what: format!("reusable workflow {u}") }));
        }
        if let Some(Y::Sequence(steps)) = j.get("steps") {
            for (i, st) in steps.iter().enumerate() {
                base.steps.push(parse_step(st, i, &mut p.warnings, &id));
            }
        }
        if let Some(c) = j.get("concurrency") {
            let _ = c;
        }

        let matrix = j.get("strategy").and_then(|st| st.get("matrix"));
        match matrix {
            Some(Y::Mapping(m)) => {
                let combos = expand_matrix(m);
                if combos.is_empty() {
                    p.jobs.push(base);
                } else {
                    for combo in combos {
                        let mut jb = base.clone();
                        let vals: Vec<String> = combo.iter().map(|(_, v)| label(v)).collect();
                        jb.id = format!("{id} ({})", vals.join(", "));
                        jb.name = format!("{base_name} ({})", vals.join(", "));
                        jb.matrix = combo.iter().map(|(k, v)| Kv::new(k.clone(), v.to_string())).collect();
                        p.jobs.push(jb);
                    }
                }
            }
            Some(Y::String(_)) => {
                p.warnings.push(format!("job `{id}` builds its matrix from an expression (fromJSON); it runs as a single job"));
                p.jobs.push(base);
            }
            _ => p.jobs.push(base),
        }
    }
    if p.jobs.is_empty() {
        return Err("no jobs found in this workflow".into());
    }
    Ok(p)
}

