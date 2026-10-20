use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build a `Command` that never pops up a console window on Windows.
pub fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Best human readable error text for a failed command.
    pub fn error_text(&self) -> String {
        let e = self.stderr.trim();
        if !e.is_empty() {
            e.to_string()
        } else {
            self.stdout.trim().to_string()
        }
    }
}

pub fn run(program: &str, args: &[&str], cwd: Option<&str>) -> Result<Out, String> {
    let mut c = command(program);
    c.args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = c.output().map_err(|e| format!("could not run `{program}`: {e}"))?;
    Ok(Out {
        ok: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

/// Remove ANSI escape sequences (colors, cursor moves) from a log line.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: until BEL or ESC \
                    chars.next();
                    for n in chars.by_ref() {
                        if n == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

pub fn detect_level(text: &str) -> &'static str {
    let up = text.to_uppercase();
    if up.contains("FATAL") || up.contains("ERROR") || up.contains("PANIC") || up.contains("EADDRINUSE") {
        "error"
    } else if up.contains("WARN") {
        "warn"
    } else if up.contains("DEBUG") || up.contains("TRACE") {
        "debug"
    } else {
        "info"
    }
}

pub fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect()
}
