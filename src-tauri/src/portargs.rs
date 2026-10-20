//! Makes a configured port actually reach the dev server.
//!
//! `PORT` in the environment is only honoured by some stacks (Express, Nest, CRA...). Vite, Next, Angular,
//! Nuxt and friends need a CLI flag, so we work out which tool the start command runs and append it.

use serde::Serialize;
use std::path::Path;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// The command that will really be executed.
    pub command: String,
    /// Human-readable explanation of what was changed (empty if nothing).
    pub notes: Vec<String>,
}

/// Flags for a known dev-server CLI, given the tokens of the command that launches it.
fn tool_flags(tokens: &[String], port: u16) -> Option<(String, String)> {
    let mut it = tokens.iter().filter(|t| !t.contains('=') && t.as_str() != "cross-env" && t.as_str() != "npx" && t.as_str() != "--");
    let first = it.next()?;
    let tool = first.rsplit(['/', '\\']).next().unwrap_or(first).trim_end_matches(".cmd").trim_end_matches(".exe").to_lowercase();
    let rest: Vec<&String> = it.collect();
    let has = |w: &str| rest.iter().any(|t| t.as_str() == w);
    let p = port.to_string();
    let flags = match tool.as_str() {
        "vite" => format!("--port {p} --strictPort"),
        "next" => format!("-p {p}"),
        "nuxt" | "nuxi" | "astro" | "vue-cli-service" | "webpack-dev-server" => format!("--port {p}"),
        "ng" if has("serve") || has("s") => format!("--port {p}"),
        "webpack" if has("serve") || has("s") => format!("--port {p}"),
        "storybook" => format!("-p {p}"),
        "http-server" => format!("-p {p}"),
        "serve" => format!("-l {p}"),
        "uvicorn" => format!("--port {p}"),
        "flask" => format!("--port {p}"),
        _ => return None,
    };
    Some((tool, flags))
}

fn tokens(s: &str) -> Vec<String> {
    s.split_whitespace().map(|t| t.to_string()).collect()
}

fn script_body(repo: &str, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(Path::new(repo).join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("scripts")?.get(name)?.as_str().map(|s| s.to_string())
}

/// If `body` is one simple tool invocation (no chaining), return it as tokens.
fn simple_invocation(body: &str) -> Option<Vec<String>> {
    let b = body.trim();
    if b.contains("&&") || b.contains("||") || b.contains(';') || b.contains('|') || b.contains("concurrently") || b.contains("npm-run-all") || b.contains("run-p ") || b.contains("run-s ") {
        return None;
    }
    Some(tokens(b))
}

fn already_sets_port(cmd: &str) -> bool {
    let lower = format!(" {} ", cmd.to_lowercase());
    ["--port", " -p ", " -l ", "--listen", "--host-port"].iter().any(|m| lower.contains(m))
}

/// Replace `{port}` style placeholders. Returns None if the command has none.
fn substitute(cmd: &str, port: u16) -> Option<String> {
    let p = port.to_string();
    let mut out = cmd.to_string();
    let mut hit = false;
    for ph in ["{{port}}", "{port}", "${PORT}", "$PORT", "%PORT%"] {
        if out.contains(ph) {
            out = out.replace(ph, &p);
            hit = true;
        }
    }
    hit.then_some(out)
}

pub fn apply(repo: &str, command: &str, port: Option<u16>) -> Applied {
    let cmd = command.trim();
    let mut a = Applied { command: cmd.to_string(), notes: vec![] };
    let Some(port) = port else { return a };
    if cmd.is_empty() {
        return a;
    }
    a.notes.push(format!("PORT={port} is set in the environment."));

    if let Some(s) = substitute(cmd, port) {
        a.command = s;
        a.notes.push("Placeholder in the command replaced with the port.".into());
        return a;
    }
    if already_sets_port(cmd) {
        a.notes.push("The command already passes a port flag, so it is left as is.".into());
        return a;
    }

    let toks = tokens(cmd);
    let first = toks.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let is_pm = matches!(first.as_str(), "npm" | "yarn" | "pnpm" | "bun");

    // Package-manager script: find the underlying tool in package.json.
    if is_pm {
        let script = match (first.as_str(), toks.get(1).map(|s| s.as_str()), toks.get(2)) {
            (_, Some("run" | "run-script"), Some(n)) => Some(n.clone()),
            ("npm", Some("start"), _) => Some("start".to_string()),
            (_, Some(n), _) if !n.starts_with('-') && !matches!(n, "install" | "add" | "exec" | "dlx" | "x") => Some(n.to_string()),
            _ => None,
        };
        if let Some(name) = script {
            if let Some(body) = script_body(repo, &name) {
                if let Some(inv) = simple_invocation(&body) {
                    if let Some((tool, flags)) = tool_flags(&inv, port) {
                        a.command = if first == "npm" {
                            if cmd.contains(" -- ") { format!("{cmd} {flags}") } else { format!("{cmd} -- {flags}") }
                        } else {
                            format!("{cmd} {flags}")
                        };
                        a.notes.push(format!("`{name}` runs {tool}, which ignores PORT: appended `{flags}`."));
                        return a;
                    }
                } else {
                    a.notes.push(format!("`{name}` chains several commands: put `{{port}}` in the command to say where the port goes."));
                    return a;
                }
            }
        }
        a.notes.push("Reads PORT from the environment (works for Express, Nest, CRA...). If your tool ignores it, add `--port {port}` to the command.".into());
        return a;
    }

    // Direct tool invocation, e.g. `vite` or `next dev`.
    if let Some((tool, flags)) = tool_flags(&toks, port) {
        a.command = format!("{cmd} {flags}");
        a.notes.push(format!("{tool} ignores PORT: appended `{flags}`."));
        return a;
    }
    a.notes.push("Reads PORT from the environment. If your tool ignores it, add `--port {port}` to the command.".into());
    a
}
