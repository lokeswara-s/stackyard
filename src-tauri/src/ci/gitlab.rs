//! `.gitlab-ci.yml` -> Pipeline.

use serde_yaml::{Mapping, Value as Y};
use std::path::Path;

use super::ir::*;

/// Separator between independent commands of a GitLab `script:` (each is echoed as `$ cmd`).
pub const SEP: char = '\u{1e}';

const RESERVED: &[&str] = &[
    "stages", "variables", "default", "include", "workflow", "image", "services", "before_script", "after_script", "cache", "artifacts", "retry", "timeout",
    "interruptible", "tags", "pages_url", "spec",
];

pub fn s(v: &Y) -> Option<String> {
    match v {
        Y::String(x) => Some(x.clone()),
        Y::Number(n) => Some(n.to_string()),
        Y::Bool(b) => Some(b.to_string()),
        Y::Null => Some(String::new()),
        _ => None,
    }
}

pub fn list_of_str(v: Option<&Y>) -> Vec<String> {
    match v {
        None => vec![],
        Some(Y::Sequence(a)) => a.iter().flat_map(|x| list_of_str(Some(x))).collect(),
        Some(x) => s(x).into_iter().collect(),
    }
}

fn get<'a>(m: &'a Y, k: &str) -> Option<&'a Y> {
    m.get(k)
}

/// Seconds from `3600`, `30m`, `1h 30m`, `2 hours`, `1 hour 30 minutes`.
pub fn parse_duration(t: &str) -> Option<u64> {
    let t = t.trim();
    if let Ok(n) = t.parse::<u64>() {
        return Some(n);
    }
    let mut total = 0u64;
    let mut num = String::new();
    let mut unit = String::new();
    let flush = |num: &mut String, unit: &mut String, total: &mut u64| -> bool {
        if num.is_empty() {
            return unit.is_empty();
        }
        let n: f64 = match num.parse() {
            Ok(n) => n,
            Err(_) => return false,
        };
        let mult = match unit.as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86400.0,
            "w" | "week" | "weeks" => 604800.0,
            _ => return false,
        };
        *total += (n * mult) as u64;
        num.clear();
        unit.clear();
        true
    };
    for ch in t.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            if !unit.is_empty() && !flush(&mut num, &mut unit, &mut total) {
                return None;
            }
            num.push(ch);
        } else if ch.is_alphabetic() {
            unit.push(ch);
        } else if ch.is_whitespace() || ch == ',' {
            if !unit.is_empty() && !flush(&mut num, &mut unit, &mut total) {
                return None;
            }
        }
    }
    if !flush(&mut num, &mut unit, &mut total) {
        return None;
    }
    Some(total)
}

fn merge_into(base: &mut Y, over: &Y) {
    match (base, over) {
        (Y::Mapping(b), Y::Mapping(o)) => {
            for (k, v) in o {
                if let Some(bv) = b.get_mut(k) {
                    merge_into(bv, v);
                } else {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

fn lookup_path(root: &Y, path: &[String]) -> Option<Y> {
    let mut cur = root;
    for p in path {
        cur = cur.get(p.as_str())?;
    }
    Some(cur.clone())
}

/// Replace `!reference [job, key]` tags with the value they point at.
fn resolve_refs(v: &Y, root: &Y, depth: u32) -> Y {
    if depth > 10 {
        return v.clone();
    }
    match v {
        Y::Tagged(t) if t.tag.to_string().trim_start_matches('!') == "reference" => {
            let path = list_of_str(Some(&t.value));
            match lookup_path(root, &path) {
                Some(found) => resolve_refs(&found, root, depth + 1),
                None => Y::Null,
            }
        }
        Y::Sequence(a) => {
            let mut out = vec![];
            for x in a {
                let r = resolve_refs(x, root, depth + 1);
                // A reference to a list splices into the surrounding list.
                let was_ref = matches!(x, Y::Tagged(t) if t.tag.to_string().trim_start_matches('!') == "reference");
                match (was_ref, r) {
                    (true, Y::Sequence(items)) => out.extend(items),
                    (_, r) => out.push(r),
                }
            }
            Y::Sequence(out)
        }
        Y::Mapping(m) => {
            let mut out = Mapping::new();
            for (k, x) in m {
                out.insert(k.clone(), resolve_refs(x, root, depth + 1));
            }
            Y::Mapping(out)
        }
        other => other.clone(),
    }
}

fn load_file(repo: &Path, rel: &str, depth: u32, warnings: &mut Vec<String>) -> Result<Y, String> {
    let path = repo.join(rel.trim_start_matches('/'));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut doc: Y = serde_yaml::from_str(&text).map_err(|e| format!("{rel}: {e}"))?;
    doc.apply_merge().map_err(|e| format!("{rel}: {e}"))?;
    let mut merged = Y::Mapping(Mapping::new());
    if let Some(inc) = doc.get("include").cloned() {
        let items: Vec<Y> = match inc {
            Y::Sequence(a) => a,
            other => vec![other],
        };
        for it in items {
            let local = match &it {
                Y::String(p) if !p.starts_with("http") => Some(p.clone()),
                Y::Mapping(_) => it.get("local").and_then(s),
                _ => None,
            };
            match local {
                Some(l) if depth < 6 => match load_file(repo, &l, depth + 1, warnings) {
                    Ok(inc_doc) => merge_into(&mut merged, &inc_doc),
                    Err(e) => warnings.push(format!("include `{l}` failed: {e}")),
                },
                Some(_) => warnings.push("include nesting is too deep".into()),
                None => warnings.push(format!(
                    "`include` of {} cannot be fetched locally and was skipped: jobs defined there are missing",
                    s(&it).or_else(|| it.get("project").and_then(s)).or_else(|| it.get("remote").and_then(s)).or_else(|| it.get("template").and_then(s)).unwrap_or_else(|| "a remote file".into())
                )),
            }
        }
    }
    merge_into(&mut merged, &doc);
    Ok(merged)
}

fn kv_map(v: Option<&Y>) -> Vec<Kv> {
    let mut out = vec![];
    if let Some(Y::Mapping(m)) = v {
        for (k, val) in m {
            if let Some(k) = s(k) {
                let value = match val {
                    Y::Mapping(_) => val.get("value").and_then(s).unwrap_or_default(),
                    other => s(other).unwrap_or_default(),
                };
                put(&mut out, &k, value);
            }
        }
    }
    out
}

fn ref_expr(r: &str) -> String {
    let r = r.trim();
    match r {
        "branches" => "$CI_COMMIT_BRANCH".into(),
        "tags" => "$CI_COMMIT_TAG".into(),
        "merge_requests" => "$CI_PIPELINE_SOURCE == \"merge_request_event\"".into(),
        "pushes" => "$CI_PIPELINE_SOURCE == \"push\"".into(),
        "schedules" => "$CI_PIPELINE_SOURCE == \"schedule\"".into(),
        "web" => "$CI_PIPELINE_SOURCE == \"web\"".into(),
        "api" => "$CI_PIPELINE_SOURCE == \"api\"".into(),
        "triggers" => "$CI_PIPELINE_SOURCE == \"trigger\"".into(),
        "external" => "$CI_PIPELINE_SOURCE == \"external\"".into(),
        _ if r.starts_with('/') && r.len() > 1 => {
            let end = r.rfind('/').unwrap_or(0);
            let flags = if end + 1 < r.len() { &r[end + 1..] } else { "" };
            format!("$CI_COMMIT_REF_NAME =~ /{}/{}", &r[1..end.max(1)], flags)
        }
        _ => format!("$CI_COMMIT_REF_NAME == \"{}\"", r.replace('"', "\\\"")),
    }
}

/// `only`/`except` -> one boolean expression.
fn filter_expr(v: &Y) -> Option<String> {
    let (refs, vars, changes): (Vec<String>, Vec<String>, Vec<String>) = match v {
        Y::Mapping(_) => (list_of_str(v.get("refs")), list_of_str(v.get("variables")), list_of_str(v.get("changes"))),
        other => (list_of_str(Some(other)), vec![], vec![]),
    };
    let _ = changes;
    let mut parts = vec![];
    if !refs.is_empty() {
        parts.push(format!("({})", refs.iter().map(|r| ref_expr(r)).collect::<Vec<_>>().join(" || ")));
    }
    if !vars.is_empty() {
        parts.push(format!("({})", vars.join(" || ")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

fn parse_rules(job: &Y) -> Vec<Rule> {
    let mut out = vec![];
    if let Some(Y::Sequence(rs)) = job.get("rules") {
        for r in rs {
            let changes = match r.get("changes") {
                Some(Y::Mapping(_)) => list_of_str(r.get("changes").and_then(|c| c.get("paths"))),
                other => list_of_str(other),
            };
            out.push(Rule {
                cond: r.get("if").and_then(s),
                changes,
                exists: list_of_str(r.get("exists")),
                when: r.get("when").and_then(s),
                allow_failure: r.get("allow_failure").and_then(|v| v.as_bool()),
                variables: kv_map(r.get("variables")),
            });
        }
        return out;
    }
    // Legacy only/except, expressed as rules.
    if let Some(e) = job.get("except").and_then(filter_expr) {
        out.push(Rule { cond: Some(e), when: Some("never".into()), ..Default::default() });
        if job.get("only").is_none() {
            out.push(Rule::default());
        }
    }
    if let Some(o) = job.get("only").and_then(filter_expr) {
        out.push(Rule { cond: Some(o), ..Default::default() });
    }
    out
}

fn parse_services(v: Option<&Y>) -> Vec<ServiceDef> {
    let mut out = vec![];
    if let Some(Y::Sequence(a)) = v {
        for it in a {
            let (image, alias, command, env) = match it {
                Y::Mapping(_) => (it.get("name").and_then(s).unwrap_or_default(), it.get("alias").and_then(s), it.get("command").map(|c| list_of_str(Some(c)).join(" ")), kv_map(it.get("variables"))),
                other => (s(other).unwrap_or_default(), None, None, vec![]),
            };
            if image.is_empty() {
                continue;
            }
            // GitLab derives the hostname from the image: `registry/ns/postgres:15` -> `ns-postgres` and `ns__postgres`; we use the last path part.
            let derived = {
                let base = image.split(':').next().unwrap_or(&image);
                let base = base.rsplit('/').next().unwrap_or(base);
                base.to_string()
            };
            out.push(ServiceDef { image, alias: alias.unwrap_or(derived), env, command, options: vec![], ports: vec![] });
        }
    }
    out
}

fn parse_cache(v: Option<&Y>) -> Vec<CacheDef> {
    let items: Vec<&Y> = match v {
        Some(Y::Sequence(a)) => a.iter().collect(),
        Some(x @ Y::Mapping(_)) => vec![x],
        _ => vec![],
    };
    items
        .into_iter()
        .map(|c| {
            let key = match c.get("key") {
                Some(Y::Mapping(_)) => {
                    let k = c.get("key").unwrap();
                    format!("files:{};prefix:{}", list_of_str(k.get("files")).join(","), k.get("prefix").and_then(s).unwrap_or_default())
                }
                Some(k) => s(k).unwrap_or_else(|| "default".into()),
                None => "default".into(),
            };
            CacheDef { key, paths: list_of_str(c.get("paths")), policy: c.get("policy").and_then(s).unwrap_or_else(|| "pull-push".into()) }
        })
        .collect()
}

fn script_entries(v: Option<&Y>) -> Vec<String> {
    match v {
        Some(Y::Sequence(a)) => a.iter().flat_map(|x| script_entries(Some(x))).collect(),
        Some(x) => s(x).filter(|t| !t.trim().is_empty()).into_iter().collect(),
        None => vec![],
    }
}

fn cartesian(m: &Mapping) -> Vec<Vec<(String, String)>> {
    let mut acc: Vec<Vec<(String, String)>> = vec![vec![]];
    for (k, v) in m {
        let key = s(k).unwrap_or_default();
        let vals = list_of_str(Some(v));
        let mut next = vec![];
        for base in &acc {
            for val in &vals {
                let mut b = base.clone();
                b.push((key.clone(), val.clone()));
                next.push(b);
            }
        }
        acc = next;
    }
    acc
}

pub fn parse(repo: &Path, file: &str) -> Result<Pipeline, String> {
    let mut warnings = vec![];
    let mut doc = load_file(repo, file, 0, &mut warnings)?;
    let root = doc.clone();
    doc = resolve_refs(&doc, &root, 0);
    let map = match &doc {
        Y::Mapping(m) => m.clone(),
        _ => return Err("the file is not a YAML mapping".into()),
    };

    let mut p = Pipeline { provider: "gitlab".into(), file: file.into(), name: ".gitlab-ci.yml".into(), gated_by_stage: true, ..Default::default() };
    p.env = kv_map(doc.get("variables"));
    let default = doc.get("default").cloned().unwrap_or(Y::Null);
    // Legacy top-level keys act as defaults too.
    let mut defaults = Y::Mapping(Mapping::new());
    for k in ["image", "services", "before_script", "after_script", "cache", "artifacts", "retry", "timeout"] {
        if let Some(v) = doc.get(k) {
            merge_into(&mut defaults, &Y::Mapping(Mapping::from_iter([(Y::String(k.into()), v.clone())])));
        }
    }
    merge_into(&mut defaults, &default);

    let stages: Vec<String> = match doc.get("stages") {
        Some(v) => list_of_str(Some(v)),
        None => vec![".pre".into(), "build".into(), "test".into(), "deploy".into(), ".post".into()],
    };

    // Job templates by name for `extends`.
    let lookup = |name: &str| -> Option<Y> { map.get(name).cloned() };
    fn flatten(name: &str, lookup: &dyn Fn(&str) -> Option<Y>, depth: u32) -> Option<Y> {
        let job = lookup(name)?;
        if depth > 10 {
            return Some(job);
        }
        let parents = list_of_str(job.get("extends"));
        let mut base = Y::Mapping(Mapping::new());
        for p in parents {
            if let Some(pj) = flatten(&p, lookup, depth + 1) {
                merge_into(&mut base, &pj);
            }
        }
        merge_into(&mut base, &job);
        Some(base)
    }

    let mut used_stages: Vec<String> = vec![];
    for (k, _) in &map {
        let name = match s(k) {
            Some(n) => n,
            None => continue,
        };
        if RESERVED.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        let Some(mut j) = flatten(&name, &lookup, 0) else { continue };
        if !matches!(j, Y::Mapping(_)) {
            continue;
        }
        if j.get("script").is_none() && j.get("trigger").is_none() && j.get("run").is_none() {
            warnings.push(format!("`{name}` has no script and was ignored"));
            continue;
        }
        // Apply `default:` for anything the job did not set.
        let mut with_defaults = defaults.clone();
        if let (Y::Mapping(d), Y::Mapping(jm)) = (&mut with_defaults, &j) {
            for (k, v) in jm {
                d.insert(k.clone(), v.clone());
            }
            j = Y::Mapping(d.clone());
        }

        let stage = j.get("stage").and_then(s).unwrap_or_else(|| "test".into());
        if !stages.contains(&stage) {
            warnings.push(format!("job `{name}` uses stage `{stage}` which is not listed under `stages:`"));
        }
        if !used_stages.contains(&stage) {
            used_stages.push(stage.clone());
        }

        let image = match j.get("image") {
            Some(Y::Mapping(_)) => j.get("image").and_then(|i| i.get("name")).and_then(s),
            Some(v) => s(v),
            None => None,
        }
        .filter(|i| !i.is_empty());

        let mut base = Job::new(name.clone(), stage);
        base.image = image;
        base.services = parse_services(j.get("services"));
        base.env = kv_map(j.get("variables"));
        base.rules = parse_rules(&j);
        base.when = j.get("when").and_then(s).unwrap_or_else(|| "on_success".into());
        base.allow_failure = match j.get("allow_failure") {
            Some(Y::Bool(b)) => *b,
            Some(Y::Mapping(_)) => true,
            _ => base.when == "manual" && base.rules.is_empty(),
        };
        base.retry = match j.get("retry") {
            Some(Y::Mapping(_)) => j.get("retry").and_then(|r| r.get("max")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            Some(v) => v.as_u64().unwrap_or(0) as u32,
            None => 0,
        };
        base.timeout_secs = j.get("timeout").and_then(s).and_then(|t| parse_duration(&t));
        base.cache = parse_cache(j.get("cache"));
        base.coverage = j.get("coverage").and_then(s);
        base.trigger = j.get("trigger").is_some();
        if let Some(a) = j.get("artifacts") {
            base.artifacts = Some(Artifacts {
                paths: list_of_str(a.get("paths")),
                exclude: list_of_str(a.get("exclude")),
                when: a.get("when").and_then(s).unwrap_or_else(|| "on_success".into()),
                name: a.get("name").and_then(s),
                dotenv: a.get("reports").and_then(|r| r.get("dotenv")).and_then(|d| list_of_str(Some(d)).into_iter().next()),
            });
        }
        base.dependencies = j.get("dependencies").map(|d| list_of_str(Some(d)));
        if let Some(n) = j.get("needs") {
            base.has_needs = true;
            if let Y::Sequence(items) = n {
                for it in items {
                    match it {
                        Y::Mapping(_) => {
                            if it.get("project").is_some() {
                                warnings.push(format!("`{name}` needs a job from another project; skipped"));
                                continue;
                            }
                            if let Some(job) = it.get("job").and_then(s) {
                                base.needs.push(Need { job, artifacts: it.get("artifacts").and_then(|a| a.as_bool()).unwrap_or(true), optional: it.get("optional").and_then(|a| a.as_bool()).unwrap_or(false) });
                            }
                        }
                        other => {
                            if let Some(job) = s(other) {
                                base.needs.push(Need { job, artifacts: true, optional: false });
                            }
                        }
                    }
                }
            }
        }
        if base.trigger {
            warnings.push(format!("`{name}` is a trigger job (downstream pipeline); it is shown but not run"));
        }

        // Scripts.
        let before = script_entries(j.get("before_script"));
        let main = script_entries(j.get("script"));
        let after = script_entries(j.get("after_script"));
        let mut all = before;
        all.extend(main);
        if !all.is_empty() {
            base.steps.push(Step::script("script", all.join(&SEP.to_string()), "gitlab"));
        }
        if !after.is_empty() {
            let mut st = Step::script("after_script", after.join(&SEP.to_string()), "gitlab");
            st.always = true;
            base.steps.push(st);
        }

        // parallel: N | parallel: { matrix: [...] }
        match j.get("parallel") {
            Some(Y::Number(n)) if n.as_u64().unwrap_or(0) > 1 => {
                let total = n.as_u64().unwrap() as u32;
                for i in 1..=total {
                    let mut jb = base.clone();
                    jb.id = format!("{name} {i}/{total}");
                    jb.name = jb.id.clone();
                    jb.node = Some((i, total));
                    p.jobs.push(jb);
                }
            }
            Some(par @ Y::Mapping(_)) => {
                let mut any = false;
                if let Some(Y::Sequence(ms)) = par.get("matrix") {
                    for m in ms {
                        if let Y::Mapping(mm) = m {
                            for combo in cartesian(mm) {
                                let mut jb = base.clone();
                                let label = combo.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>().join(", ");
                                jb.id = format!("{name}: [{label}]");
                                jb.name = jb.id.clone();
                                for (k, v) in &combo {
                                    put(&mut jb.env, k, v.clone());
                                    jb.matrix.push(Kv::new(k, v));
                                }
                                p.jobs.push(jb);
                                any = true;
                            }
                        }
                    }
                }
                if !any {
                    p.jobs.push(base);
                }
            }
            _ => p.jobs.push(base),
        }
    }

    let mut ordered: Vec<String> = stages.into_iter().filter(|st| used_stages.contains(st)).collect();
    for st in used_stages {
        if !ordered.contains(&st) {
            ordered.push(st);
        }
    }
    p.stages = ordered;
    p.warnings = warnings;
    if p.jobs.is_empty() {
        return Err("no jobs found in this file".into());
    }
    Ok(p)
}
