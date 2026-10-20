//! Running scripts: in a Docker container (like a server runner) or directly on this machine.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::ir::Kv;
use crate::util::{command, run};

pub fn docker_ready() -> bool {
    static CACHE: OnceLock<Mutex<(Instant, bool)>> = OnceLock::new();
    let c = CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(60), false)));
    let mut g = c.lock().unwrap();
    if g.0.elapsed() > Duration::from_secs(5) {
        *g = (Instant::now(), crate::docker::is_running());
    }
    g.1
}

/// A bash that understands POSIX scripts: Git Bash on Windows, `bash` elsewhere.
pub fn host_bash() -> Option<String> {
    static B: OnceLock<Option<String>> = OnceLock::new();
    B.get_or_init(|| {
        #[cfg(windows)]
        {
            let mut cands: Vec<PathBuf> = vec![];
            for v in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
                if let Ok(base) = std::env::var(v) {
                    cands.push(Path::new(&base).join("Git").join("bin").join("bash.exe"));
                    cands.push(Path::new(&base).join("Programs").join("Git").join("bin").join("bash.exe"));
                }
            }
            if let Ok(o) = run("git", &["--exec-path"], None) {
                // <git>/mingw64/libexec/git-core -> <git>/bin/bash.exe
                let p = PathBuf::from(o.stdout.trim());
                if let Some(root) = p.ancestors().nth(3) {
                    cands.push(root.join("bin").join("bash.exe"));
                }
            }
            cands.into_iter().find(|p| p.exists()).map(|p| p.to_string_lossy().into_owned())
        }
        #[cfg(not(windows))]
        {
            Some("bash".to_string())
        }
    })
    .clone()
}

/// Kill a process and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = command("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(not(windows))]
    {
        let _ = command("pkill").args(["-TERM", "-P", &pid.to_string()]).status();
        let _ = command("kill").args(["-9", &pid.to_string()]).status();
    }
}

/// Run `cmd`, calling `emit` for every output line (stdout and stderr merged). Returns the exit code.
pub fn run_streaming(mut cmd: Command, cancel: &AtomicBool, timeout: Option<Duration>, emit: &mut dyn FnMut(&str)) -> Result<i32, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start: {e}"))?;
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = vec![];
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || pump(out, tx)));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || pump(err, tx)));
    }
    drop(tx);
    let started = Instant::now();
    let mut killed: Option<&str> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(line) => emit(&line),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if killed.is_none() {
                    if cancel.load(Ordering::SeqCst) {
                        killed = Some("cancelled");
                    } else if timeout.map(|t| started.elapsed() > t).unwrap_or(false) {
                        killed = Some("timeout");
                    }
                    if killed.is_some() {
                        kill_tree(child.id());
                        let _ = child.kill();
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    for r in readers {
        let _ = r.join();
    }
    match killed {
        Some("timeout") => Err("timeout".into()),
        Some(_) => Err("cancelled".into()),
        None => Ok(status.code().unwrap_or(-1)),
    }
}

fn pump(r: impl std::io::Read, tx: mpsc::Sender<String>) {
    let mut br = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match br.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let s = String::from_utf8_lossy(&buf);
                let s = s.trim_end_matches(['\n', '\r']);
                // Progress bars rewrite the line with \r: keep the last state.
                let s = s.rsplit('\r').next().unwrap_or(s);
                if tx.send(s.to_string()).is_err() {
                    break;
                }
            }
        }
    }
}

// ============================== docker helpers ==============================

pub fn docker(args: &[&str]) -> Result<crate::util::Out, String> {
    run("docker", args, None)
}

pub fn image_present(image: &str) -> bool {
    docker(&["image", "inspect", image]).map(|o| o.ok).unwrap_or(false)
}

static PULL_LOCK: Mutex<()> = Mutex::new(());

pub fn ensure_image(image: &str, cancel: &AtomicBool, emit: &mut dyn FnMut(&str)) -> Result<(), String> {
    let _g = PULL_LOCK.lock().unwrap();
    if image_present(image) {
        return Ok(());
    }
    emit(&format!("Pulling image {image} (first use; this can take a while)"));
    let mut c = command("docker");
    c.args(["pull", image]);
    let mut last = Instant::now() - Duration::from_secs(10);
    let code = run_streaming(c, cancel, Some(Duration::from_secs(60 * 30)), &mut |l| {
        // Keep the log readable: layer progress is chatty.
        if l.contains("Pull complete") || l.starts_with("Status:") || l.starts_with("Digest:") || last.elapsed() > Duration::from_secs(5) {
            emit(l);
            last = Instant::now();
        }
    })?;
    if code == 0 {
        Ok(())
    } else {
        Err(format!("could not pull image `{image}`"))
    }
}

pub fn remove_container(name: &str) {
    let _ = docker(&["rm", "-f", "-v", name]);
}

pub fn remove_network(name: &str) {
    let _ = docker(&["network", "rm", name]);
}

pub fn create_network(name: &str) -> Result<(), String> {
    let o = docker(&["network", "create", name])?;
    if o.ok {
        Ok(())
    } else {
        Err(o.error_text())
    }
}

/// Remove containers and networks left behind by a crash.
pub fn cleanup_leftovers() {
    crate::docker::cleanup_managed("dwci-");
    if let Ok(o) = docker(&["network", "ls", "-q", "--filter", "name=^dwci-"]) {
        for id in o.stdout.split_whitespace() {
            remove_network(id);
        }
    }
}

pub struct ContainerSpec<'a> {
    pub name: &'a str,
    pub image: &'a str,
    pub network: Option<&'a str>,
    pub aliases: Vec<String>,
    pub mounts: Vec<(String, String, bool)>,
    pub workdir: Option<&'a str>,
    pub env: Vec<Kv>,
    pub args: Vec<String>,
    pub cmd: Vec<String>,
    pub keep_alive: bool,
    pub privileged: bool,
}

pub fn start_container(s: &ContainerSpec) -> Result<(), String> {
    let mut a: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), s.name.into(), "--label".into(), "stackyard=ci".into()];
    if let Some(n) = s.network {
        a.push("--network".into());
        a.push(n.into());
        for al in &s.aliases {
            a.push("--network-alias".into());
            a.push(al.clone());
        }
    }
    for (h, c, ro) in &s.mounts {
        a.push("-v".into());
        a.push(format!("{h}:{c}{}", if *ro { ":ro" } else { "" }));
    }
    if let Some(w) = s.workdir {
        a.push("-w".into());
        a.push(w.into());
    }
    for kv in &s.env {
        a.push("-e".into());
        a.push(format!("{}={}", kv.key, kv.value));
    }
    if s.privileged {
        a.push("--privileged".into());
    }
    a.extend(s.args.iter().cloned());
    if s.keep_alive {
        a.push("--entrypoint".into());
        a.push("tail".into());
    }
    a.push(s.image.into());
    if s.keep_alive {
        a.push("-f".into());
        a.push("/dev/null".into());
    } else {
        a.extend(s.cmd.iter().cloned());
    }
    let refs: Vec<&str> = a.iter().map(|x| x.as_str()).collect();
    // Docker Desktop occasionally drops the connection under load ("error during connect ... EOF"): retry those.
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(1500));
            remove_container(s.name);
        }
        let o = docker(&refs)?;
        if o.ok {
            return Ok(());
        }
        last = o.error_text();
        if !(last.contains("error during connect") || last.contains("EOF") || last.contains("connection reset")) {
            break;
        }
    }
    Err(last)
}

pub fn container_running(name: &str) -> bool {
    docker(&["inspect", "-f", "{{.State.Running}}", name]).map(|o| o.ok && o.stdout.trim() == "true").unwrap_or(false)
}

pub fn health_status(name: &str) -> Option<String> {
    let o = docker(&["inspect", "-f", "{{if .State.Health}}{{.State.Health.Status}}{{end}}", name]).ok()?;
    let t = o.stdout.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

pub fn exposed_ports(image: &str) -> Vec<u16> {
    let o = match docker(&["image", "inspect", "-f", "{{range $p, $_ := .Config.ExposedPorts}}{{$p}} {{end}}", image]) {
        Ok(o) if o.ok => o,
        _ => return vec![],
    };
    o.stdout.split_whitespace().filter_map(|p| p.split('/').next()?.parse().ok()).collect()
}

// ============================== sandbox ==============================

#[derive(Clone, Debug)]
pub enum Shell {
    /// `bash --noprofile --norc -eo pipefail`
    Bash,
    /// `sh -e`
    Sh,
    /// Jenkins `sh`: `sh -xe`
    JenkinsSh,
    /// GitLab: bash when available, else sh (the script sets its own flags).
    Auto,
    Python,
    Pwsh,
    Cmd,
    /// Template containing `{0}` for the script path.
    Custom(String),
}

impl Shell {
    pub fn parse(name: &str) -> Shell {
        match name.trim() {
            "bash" => Shell::Bash,
            "sh" => Shell::Sh,
            "python" | "python3" => Shell::Python,
            "pwsh" | "powershell" => Shell::Pwsh,
            "cmd" => Shell::Cmd,
            other => Shell::Custom(other.to_string()),
        }
    }
}

pub struct Sandbox {
    pub container: Option<String>,
    /// Project directory on this machine and as the script sees it.
    pub ws_host: PathBuf,
    pub ws_in: String,
    /// Scratch directory (scripts, runner files) on this machine and as seen inside.
    pub tmp_host: PathBuf,
    pub tmp_in: String,
    pub has_bash: bool,
    pub counter: Mutex<u32>,
}

impl Sandbox {
    pub fn is_docker(&self) -> bool {
        self.container.is_some()
    }

    /// Map a path as seen inside the sandbox to a path on this machine (for artifact copying).
    pub fn to_host(&self, p: &str) -> Option<PathBuf> {
        let norm = p.replace('\\', "/");
        let ws = self.ws_in.replace('\\', "/");
        if let Some(rest) = norm.strip_prefix(&ws) {
            return Some(self.ws_host.join(rest.trim_start_matches('/')));
        }
        let tmp = self.tmp_in.replace('\\', "/");
        if let Some(rest) = norm.strip_prefix(&tmp) {
            return Some(self.tmp_host.join(rest.trim_start_matches('/')));
        }
        None
    }

    pub fn exec(&self, script: &str, shell: &Shell, env: &[Kv], dir_in: &str, cancel: &AtomicBool, timeout: Option<Duration>, emit: &mut dyn FnMut(&str)) -> Result<i32, String> {
        let n = {
            let mut c = self.counter.lock().unwrap();
            *c += 1;
            *c
        };
        let ext = match shell {
            Shell::Python => "py",
            Shell::Pwsh => "ps1",
            Shell::Cmd => "cmd",
            _ => "sh",
        };
        let fname = format!("step-{n}.{ext}");
        let host_file = self.tmp_host.join("scripts").join(&fname);
        std::fs::create_dir_all(host_file.parent().unwrap()).map_err(|e| e.to_string())?;
        // Scripts must have unix line endings to run under sh.
        let body = if ext == "sh" { script.replace("\r\n", "\n") } else { script.to_string() };
        std::fs::write(&host_file, body).map_err(|e| e.to_string())?;
        let file_in = format!("{}/scripts/{}", self.tmp_in.trim_end_matches('/'), fname);

        let mut argv: Vec<String> = match shell {
            Shell::Bash => vec!["bash".into(), "--noprofile".into(), "--norc".into(), "-eo".into(), "pipefail".into(), file_in.clone()],
            Shell::Sh => vec!["sh".into(), "-e".into(), file_in.clone()],
            Shell::JenkinsSh => vec![if self.has_bash { "bash".into() } else { "sh".into() }, "-xe".into(), file_in.clone()],
            Shell::Auto => vec![if self.has_bash { "bash".into() } else { "sh".into() }, file_in.clone()],
            Shell::Python => vec!["python".into(), file_in.clone()],
            Shell::Pwsh => vec!["pwsh".into(), "-command".into(), format!(". '{file_in}'")],
            Shell::Cmd => vec!["cmd".into(), "/D".into(), "/E:ON".into(), "/V:OFF".into(), "/S".into(), "/C".into(), format!("CALL \"{file_in}\"")],
            Shell::Custom(t) => t.replace("{0}", &file_in).split_whitespace().map(String::from).collect(),
        };
        // One pipe for stdout and stderr, so `set -x` traces stay in order with the output (as in a real log).
        if matches!(shell, Shell::Bash | Shell::Sh | Shell::JenkinsSh | Shell::Auto) {
            let mut w: Vec<String> = vec!["sh".into(), "-c".into(), "exec \"$@\" 2>&1".into(), "sh".into()];
            w.append(&mut argv);
            argv = w;
        }

        let mut cmd;
        if let Some(c) = &self.container {
            cmd = command("docker");
            cmd.arg("exec").arg("-w").arg(dir_in);
            for kv in env {
                cmd.arg("-e").arg(format!("{}={}", kv.key, kv.value));
            }
            cmd.arg(c);
            cmd.args(&argv);
        } else {
            let mut prog = argv[0].clone();
            let args: Vec<String> = argv[1..].to_vec();
            if matches!(shell, Shell::Bash | Shell::Sh | Shell::JenkinsSh | Shell::Auto) {
                prog = host_bash().ok_or("bash was not found on this machine (install Git for Windows to run shell scripts)")?;
            }
            cmd = command(&prog);
            cmd.args(&args);
            let fallback = self.ws_host.to_string_lossy().into_owned();
            cmd.current_dir(if Path::new(dir_in).exists() { dir_in } else { fallback.as_str() });
            for kv in env {
                cmd.env(&kv.key, &kv.value);
            }
        }
        run_streaming(cmd, cancel, timeout, emit)
    }

    /// Run a command line (argv) inside the sandbox without a script file (`docker exec` / host process).
    pub fn quick(&self, argv: &[&str]) -> Option<String> {
        let out = if let Some(c) = &self.container {
            let mut a = vec!["exec", c.as_str()];
            a.extend(argv);
            run("docker", &a, None).ok()?
        } else {
            run(argv[0], &argv[1..], Some(&self.ws_host.to_string_lossy())).ok()?
        };
        if out.ok {
            Some(out.stdout)
        } else {
            None
        }
    }
}

pub fn detect_bash(container: &str) -> bool {
    run("docker", &["exec", container, "sh", "-c", "command -v bash"], None).map(|o| o.ok && !o.stdout.trim().is_empty()).unwrap_or(false)
}

/// `a b "c d"` style options into argv.
pub fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
