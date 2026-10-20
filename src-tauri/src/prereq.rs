//! Workspace prerequisites: run each `label = command` check and report whether the tool is there.

use serde::Serialize;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::ci::exec::run_streaming;
use crate::model::Prerequisite;
use crate::shell::{build, Setup};

const TIMEOUT: Duration = Duration::from_secs(15);
const OUTPUT_CAP: usize = 4000;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub label: String,
    pub command: String,
    pub expected: String,
    pub ok: bool,
    /// Exit code; `None` when the command could not start or timed out.
    pub code: Option<i32>,
    pub output: String,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Run one check in the default terminal without profiles (predictable, and `java -version` writes to stderr).
/// Passes when the exit code is 0.
pub fn check_one(p: &Prerequisite) -> CheckResult {
    let mut r = CheckResult { label: p.label.clone(), command: p.command.clone(), expected: p.expected.trim().to_string(), ..Default::default() };
    let started = Instant::now();
    let cmd = p.command.trim();
    if cmd.is_empty() {
        r.error = Some("No command to run".into());
        return r;
    }
    let built = build("", cmd, &Setup { no_profile: true, init_script: "" });
    let mut c = match built {
        Ok(c) => c,
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    };
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        c.current_dir(home);
    }
    let mut out = String::new();
    let res = run_streaming(c, &AtomicBool::new(false), Some(TIMEOUT), &mut |l| {
        if out.len() < OUTPUT_CAP {
            out.push_str(l.trim_end());
            out.push('\n');
        }
    });
    r.output = out.trim().to_string();
    r.duration_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(code) => {
            r.code = Some(code);
            r.ok = code == 0;
            if !r.ok {
                r.error = Some(format!("exited with code {code}"));
            } else if !r.expected.is_empty() && !r.output.to_lowercase().contains(&r.expected.to_lowercase()) {
                r.ok = false;
                r.error = Some(format!("output does not contain \"{}\"", r.expected));
            }
        }
        Err(e) if e == "timeout" => r.error = Some(format!("timed out after {}s", TIMEOUT.as_secs())),
        Err(e) => r.error = Some(e),
    }
    r
}

/// Run every check, one after the other, in order.
pub fn check_all(items: &[Prerequisite]) -> Vec<CheckResult> {
    items.iter().map(check_one).collect()
}
