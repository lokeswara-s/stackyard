//! The terminal a service's start command runs in: Command Prompt, PowerShell (Windows PowerShell or 7) or Bash
//! (Git Bash on Windows), optionally with the user's normal profile and an extra init script loaded first.

use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

use crate::util::command;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShellInfo {
    /// "" (default) | cmd | powershell | pwsh | bash
    pub id: String,
    pub label: String,
    pub available: bool,
    pub path: Option<String>,
    /// Where this terminal's own profile lives (what "load my profile" loads).
    pub profile: Option<String>,
}

/// How to set the terminal up before the start command runs.
#[derive(Clone, Debug, Default)]
pub struct Setup<'a> {
    /// Skip the terminal's normal profile (`$PROFILE`, `~/.bash_profile`...). By default it is loaded, like a regular terminal.
    pub no_profile: bool,
    /// A script run (sourced) in the same session just before the command: defines functions, aliases, PATH tweaks...
    pub init_script: &'a str,
}

/// First match of `names` in PATH (`.exe` is added on Windows).
fn on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for n in names {
            let c = if cfg!(windows) { format!("{n}.exe") } else { n.to_string() };
            let p = dir.join(&c);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Windows PowerShell 5.1 (`powershell.exe`): the one that loads `Documents\WindowsPowerShell\profile.ps1`.
fn windows_powershell_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let p = std::path::Path::new(&root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        if p.exists() {
            return Some(p);
        }
    }
    on_path(&["powershell"])
}

/// PowerShell 7+ (`pwsh`): loads `Documents\PowerShell\profile.ps1`.
fn pwsh_path() -> Option<PathBuf> {
    on_path(&["pwsh"])
}

fn bash_path() -> Option<String> {
    crate::ci::exec::host_bash().map(|b| if b == "bash" { on_path(&["bash"]).map(|p| p.to_string_lossy().into_owned()).unwrap_or(b) } else { b })
}

/// Ask PowerShell where its profile is (`$PROFILE.CurrentUserAllHosts`).
fn powershell_profile(exe: &std::path::Path) -> Option<String> {
    let o = crate::util::run(&exe.to_string_lossy(), &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "$PROFILE.CurrentUserAllHosts"], None).ok()?;
    let p = o.stdout.trim().to_string();
    (o.ok && !p.is_empty()).then_some(p)
}

pub fn available() -> Vec<ShellInfo> {
    let win = cfg!(windows);
    let info = |id: &str, label: &str, path: Option<String>, profile: Option<String>| ShellInfo { id: id.into(), label: label.into(), available: path.is_some(), path, profile };
    let mut v = vec![ShellInfo {
        id: String::new(),
        label: if win { "Default (Command Prompt)".into() } else { "Default (sh)".into() },
        available: true,
        path: None,
        profile: None,
    }];
    if win {
        v.push(info("cmd", "Command Prompt (cmd)", Some("cmd".into()), Some("AutoRun (registry)".into())));
        let wp = windows_powershell_path();
        let prof = wp.as_deref().and_then(powershell_profile);
        v.push(info("powershell", "Windows PowerShell", wp.map(|p| p.to_string_lossy().into_owned()), prof));
    }
    let pw = pwsh_path();
    let prof = pw.as_deref().and_then(powershell_profile);
    v.push(info("pwsh", "PowerShell 7 (pwsh)", pw.map(|p| p.to_string_lossy().into_owned()), prof));
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    v.push(info("bash", if win { "Git Bash" } else { "Bash" }, bash_path(), (!home.is_empty()).then(|| format!("{home}/.bash_profile, .bashrc"))));
    v
}

/// Standard base64 (for PowerShell's `-EncodedCommand`).
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn init_path(setup: &Setup) -> Result<Option<String>, String> {
    let p = setup.init_script.trim().trim_matches('"');
    if p.is_empty() {
        return Ok(None);
    }
    if !std::path::Path::new(p).is_file() {
        return Err(format!("Init script not found: {p}"));
    }
    Ok(Some(p.to_string()))
}

/// The wrapper PowerShell runs: UTF-8 output, quiet progress bars, the init script, and the exit code of the last native command.
fn powershell_script(cmd: &str, init: Option<&str>) -> String {
    let init = init.map(|p| format!(". '{}'\n", p.replace('\'', "''"))).unwrap_or_default();
    // A profile or init script may have run native commands: only the start command's exit code counts.
    format!("[Console]::OutputEncoding=[System.Text.Encoding]::UTF8\n$ProgressPreference='SilentlyContinue'\n{init}$global:LASTEXITCODE=$null\n{cmd}\nif ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}\n")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn own_group(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    c.process_group(0);
}
#[cfg(not(unix))]
fn own_group(_: &mut Command) {}

/// Build the process that runs `cmd` in the chosen shell. The caller sets the working directory, env and pipes.
pub fn build(shell: &str, cmd: &str, setup: &Setup) -> Result<Command, String> {
    let init = init_path(setup)?;
    match shell.trim() {
        s @ ("powershell" | "pwsh") => {
            let exe = if s == "pwsh" { pwsh_path() } else { windows_powershell_path().or_else(pwsh_path) }
                .ok_or_else(|| format!("{} was not found on this machine. Pick another terminal for this service.", if s == "pwsh" { "PowerShell 7 (pwsh)" } else { "Windows PowerShell" }))?;
            let utf16: Vec<u8> = powershell_script(cmd, init.as_deref()).encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let mut c = command(&exe.to_string_lossy());
            c.args(["-NoLogo", "-NonInteractive", "-ExecutionPolicy", "Bypass"]);
            if setup.no_profile {
                c.arg("-NoProfile");
            }
            c.args(["-EncodedCommand", &base64(&utf16)]);
            own_group(&mut c);
            Ok(c)
        }
        "bash" => {
            let exe = bash_path().ok_or_else(|| "Git Bash was not found. Install Git for Windows (it includes Bash) or pick another terminal for this service.".to_string())?;
            let mut c = command(&exe);
            let mut script = String::new();
            if let Some(p) = &init {
                script.push_str(&format!(". {}\n", sh_quote(&p.replace('\\', "/"))));
            }
            script.push_str(cmd);
            if !setup.no_profile {
                // Login shell: reads ~/.bash_profile (which normally sources ~/.bashrc). Stay in the service's folder.
                c.arg("-l").env("CHERE_INVOKING", "1");
            }
            c.arg("-c").arg(script);
            own_group(&mut c);
            Ok(c)
        }
        "cmd" | "" => {
            // cmd runs the AutoRun registry command (its "profile") on its own unless started with /D.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let mut c = command("cmd");
                if setup.no_profile {
                    c.arg("/D");
                }
                let line = match &init {
                    // cmd expands %VAR% for a whole line before running any of it, so variables set by the init
                    // script would be invisible to the command. A batch file parses line by line instead.
                    Some(p) => {
                        let dir = std::env::temp_dir().join("stackyard-shell");
                        let _ = std::fs::create_dir_all(&dir);
                        if let Ok(rd) = std::fs::read_dir(&dir) {
                            for e in rd.flatten() {
                                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|a| a.as_secs() > 86_400).unwrap_or(false);
                                if old {
                                    let _ = std::fs::remove_file(e.path());
                                }
                            }
                        }
                        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                        let file = dir.join(format!("run-{}-{}.cmd", crate::util::now_ms(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
                        std::fs::write(&file, format!("@echo off\r\ncall \"{p}\"\r\n{cmd}\r\n")).map_err(|e| format!("could not prepare the init script: {e}"))?;
                        format!("\"{}\"", file.display())
                    }
                    None => cmd.to_string(),
                };
                c.arg("/C").raw_arg(line);
                Ok(c)
            }
            #[cfg(not(windows))]
            {
                use std::os::unix::process::CommandExt;
                let mut c = command("sh");
                let line = match &init {
                    Some(p) => format!(". {}\n{cmd}", sh_quote(p)),
                    None => cmd.to_string(),
                };
                c.arg("-c").arg(line).process_group(0);
                Ok(c)
            }
        }
        other => Err(format!("Unknown terminal `{other}` (use cmd, powershell, pwsh or bash)")),
    }
}
