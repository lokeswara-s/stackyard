use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{ProcessesToUpdate, System};

use crate::chaos::{self, ChaosStats, Faults, Mode, ProbeResult, ProxyHandle};
use crate::model::{resolve_env, Service, Workspace};
use crate::ports;
use crate::up::UpRun;
use crate::util::{command, detect_level, now_ms, sanitize_name, strip_ansi};

const MAX_LOGS: usize = 30_000;
const TRIM_TO: usize = 20_000;
/// Errors older than this no longer make a running service "degraded".
const ERROR_WINDOW_MS: u64 = 60_000;
/// A stack-trace line this soon after an error line belongs to that error.
const CONTINUATION_MS: u64 = 1_500;

/// Indented lines, `at foo (file:1:2)` frames and bare closing brackets continue the previous message.
fn is_continuation(text: &str) -> bool {
    let t = text.trim();
    text.starts_with([' ', '\t']) || t.starts_with("at ") || matches!(t, "}" | "}," | "]" | "]," | "});" | ")")
}

/// Port from a `connect ECONNREFUSED 127.0.0.1:4000` / `::1:4000` style message.
fn refused_port(text: &str) -> Option<u16> {
    let rest = &text[text.find("ECONNREFUSED")? + "ECONNREFUSED".len()..];
    let token = rest.split_whitespace().next()?;
    token.rsplit(':').next()?.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub seq: u64,
    pub ts: u64,
    pub workspace: String,
    pub service: String,
    /// stdout | stderr | system
    pub stream: String,
    pub level: String,
    pub text: String,
    /// First line of an error (stack-trace continuation lines are not heads), so one error counts once.
    #[serde(skip)]
    pub head: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    pub last_seq: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRuntime {
    pub name: String,
    /// idle | starting | running | stopping | stopped | failed
    pub status: String,
    pub pid: Option<u32>,
    pub started_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub mem_mb: Option<f64>,
    pub cpu: Option<f32>,
    /// True when the service is accepting connections (on the configured port, or on a detected one).
    pub port_open: bool,
    /// Ports the service's process tree is actually listening on (detected from the OS, not from config).
    pub ports: Vec<u16>,
    /// The port to show/open: the configured one if it is open, otherwise the first detected one.
    pub live_port: Option<u16>,
    /// A port is configured but the service is listening somewhere else.
    pub port_mismatch: bool,
    /// none | starting | healthy | degraded. "Degraded" = running, but it logged errors in the last minute.
    pub health: String,
    /// Distinct errors (a stack trace counts once) since the service started, within the last minute.
    pub error_count: u32,
    pub last_error: Option<String>,
    /// Plain-language explanation, e.g. which service a refused connection was aimed at.
    pub health_hint: Option<String>,
    /// off | armed (will use the fault proxy on next start) | proxy (routed, no faults) | faulting
    pub chaos: String,
}

/// How long a service with no configured port gets to start listening before we call it running anyway.
const NO_PORT_GRACE_MS: u64 = 3000;
/// Debugger / tooling ports that must not be mistaken for the app's port.
const IGNORED_PORTS: [u16; 2] = [9229, 9230];

struct Snap {
    at: Option<Instant>,
    parents: HashMap<u32, u32>,
    listeners: Vec<(u16, u32)>,
}

struct Probe {
    open: bool,
    mismatch: bool,
    ports: Vec<u16>,
}

fn in_tree(parents: &HashMap<u32, u32>, pid: u32, root: u32) -> bool {
    let mut cur = pid;
    for _ in 0..24 {
        if cur == root {
            return true;
        }
        match parents.get(&cur) {
            Some(p) if *p != cur => cur = *p,
            _ => return false,
        }
    }
    false
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub service: String,
    pub port: u16,
    pub pid: u32,
    pub process: String,
    /// Docker container that publishes the port, when the port is held by a container.
    pub container: Option<String>,
}

pub struct StartError {
    pub message: String,
    pub conflict: Option<Conflict>,
}

impl From<String> for StartError {
    fn from(message: String) -> Self {
        StartError { message, conflict: None }
    }
}

struct Proc {
    pid: u32,
    started_ms: u64,
    alive: bool,
    stop_requested: bool,
    exit_code: Option<i32>,
    port: Option<u16>,
    container: Option<String>,
    /// Last set of listening ports we reported in the log (to log changes once).
    announced: Vec<u16>,
    infra: bool,
    /// Set when the service sits behind the fault proxy: the port clients use. `port` is then the hidden one.
    public_port: Option<u16>,
}

/// A free loopback port (briefly reserved, then released for the service to take).
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok().and_then(|l| l.local_addr().ok()).map(|a| a.port())
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChaosState {
    pub service: String,
    pub kind: String,
    /// http (per-request faults) | tcp (per-connection faults, for databases and brokers)
    pub mode: Mode,
    /// The service is configured to run behind the proxy.
    pub enabled: bool,
    /// The proxy is up right now.
    pub active: bool,
    pub public_port: Option<u16>,
    pub upstream_port: Option<u16>,
    pub faults: Faults,
    pub stats: Option<ChaosStats>,
}

pub struct Runtime {
    snap: Mutex<Snap>,
    faults: Mutex<HashMap<String, Arc<Mutex<Faults>>>>,
    proxies: Mutex<HashMap<String, ProxyHandle>>,
    last_error: Mutex<HashMap<String, u64>>,
    procs: Mutex<HashMap<String, Proc>>,
    logs: Mutex<Vec<LogLine>>,
    seq: AtomicU64,
    pub up: Mutex<Option<UpRun>>,
    pub up_cancel: AtomicBool,
    sys: Mutex<System>,
}

fn key(ws: &str, svc: &str) -> String {
    format!("{ws}/{svc}")
}

impl Runtime {
    pub fn new() -> Arc<Self> {
        Arc::new(Runtime {
            snap: Mutex::new(Snap { at: None, parents: HashMap::new(), listeners: Vec::new() }),
            last_error: Mutex::new(HashMap::new()),
            faults: Mutex::new(HashMap::new()),
            proxies: Mutex::new(HashMap::new()),
            procs: Mutex::new(HashMap::new()),
            logs: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            up: Mutex::new(None),
            up_cancel: AtomicBool::new(false),
            sys: Mutex::new(System::new()),
        })
    }

    pub fn log(&self, ws: &str, service: &str, stream: &str, text: &str) {
        let text = strip_ansi(text);
        if text.trim().is_empty() {
            return;
        }
        let now = now_ms();
        let mut level = if stream == "system" { "info" } else { detect_level(&text) };
        let mut head = false;
        if stream != "system" {
            let k = key(ws, service);
            let cont = is_continuation(&text);
            let mut last = self.last_error.lock().unwrap();
            if level == "error" {
                head = !cont;
                last.insert(k, now);
            } else if cont && last.get(&k).map_or(false, |t| now.saturating_sub(*t) <= CONTINUATION_MS) {
                // e.g. the `at …` frames and `code: 'ECONNREFUSED'` lines under an error
                level = "error";
                last.insert(k, now);
            }
        }
        let line = LogLine {
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            ts: now,
            workspace: ws.to_string(),
            service: service.to_string(),
            stream: stream.to_string(),
            level: level.to_string(),
            text,
            head,
        };
        let mut logs = self.logs.lock().unwrap();
        logs.push(line);
        if logs.len() > MAX_LOGS {
            let drop = logs.len() - TRIM_TO;
            logs.drain(0..drop);
        }
    }

    pub fn logs_after(&self, ws: &str, after: u64, limit: usize) -> LogPage {
        let logs = self.logs.lock().unwrap();
        let mut lines: Vec<LogLine> = logs.iter().filter(|l| l.seq > after && l.workspace == ws).cloned().collect();
        if lines.len() > limit {
            lines = lines.split_off(lines.len() - limit);
        }
        LogPage { lines, last_seq: self.seq.load(Ordering::SeqCst) }
    }

    pub fn clear_logs(&self, ws: &str) {
        self.logs.lock().unwrap().retain(|l| l.workspace != ws);
    }

    pub fn recent_error(&self, ws: &str, service: &str) -> Option<String> {
        let logs = self.logs.lock().unwrap();
        logs.iter()
            .rev()
            .filter(|l| l.workspace == ws && l.service == service && l.stream != "system")
            .find(|l| l.level == "error")
            .or_else(|| logs.iter().rev().find(|l| l.workspace == ws && l.service == service && l.stream != "system"))
            .map(|l| l.text.clone())
    }

    pub fn is_alive(&self, ws: &str, svc: &str) -> bool {
        self.procs.lock().unwrap().get(&key(ws, svc)).map_or(false, |p| p.alive)
    }

    /// (alive, exit_code) for a started service.
    pub fn exit_state(&self, ws: &str, svc: &str) -> Option<(bool, Option<i32>)> {
        self.procs.lock().unwrap().get(&key(ws, svc)).map(|p| (p.alive, p.exit_code))
    }

    /// Refresh (at most every 700ms) the process-parent map and the OS listener table.
    fn refresh_snapshot(&self) {
        if self.snap.lock().unwrap().at.map_or(false, |t| t.elapsed() < Duration::from_millis(700)) {
            return;
        }
        let parents: HashMap<u32, u32> = {
            let mut sys = self.sys.lock().unwrap();
            sys.refresh_processes(ProcessesToUpdate::All, true);
            sys.processes().iter().filter_map(|(p, pr)| pr.parent().map(|pp| (p.as_u32(), pp.as_u32()))).collect()
        };
        let listeners = ports::listening_pids();
        *self.snap.lock().unwrap() = Snap { at: Some(Instant::now()), parents, listeners };
    }

    /// Ports that the process tree rooted at `root` is listening on.
    fn tree_ports(&self, root: u32) -> Vec<u16> {
        self.refresh_snapshot();
        let s = self.snap.lock().unwrap();
        let mut v: Vec<u16> = s
            .listeners
            .iter()
            .filter(|(port, pid)| !IGNORED_PORTS.contains(port) && in_tree(&s.parents, *pid, root))
            .map(|(port, _)| *port)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn probe(&self, root: u32, configured: Option<u16>, infra: bool) -> Probe {
        let cfg_open = configured.map_or(false, ports::is_open);
        if infra {
            return Probe { open: cfg_open, mismatch: false, ports: vec![] };
        }
        let ports = self.tree_ports(root);
        match configured {
            Some(_) if cfg_open => Probe { open: true, mismatch: false, ports },
            Some(_) if !ports.is_empty() => Probe { open: true, mismatch: true, ports },
            Some(_) => Probe { open: false, mismatch: false, ports },
            None => Probe { open: !ports.is_empty(), mismatch: false, ports },
        }
    }

    /// Ports the service is really listening on right now.
    pub fn live_ports(&self, ws: &str, svc: &str) -> Vec<u16> {
        let found = self.procs.lock().unwrap().get(&key(ws, svc)).filter(|p| p.alive && !p.infra).map(|p| (p.pid, p.public_port));
        match found {
            // Behind the fault proxy, clients use the public port, not the hidden one.
            Some((_, Some(public))) => vec![public],
            Some((pid, None)) => self.tree_ports(pid),
            None => vec![],
        }
    }

    pub fn is_ready(&self, ws: &str, svc: &str) -> bool {
        let info = {
            let g = self.procs.lock().unwrap();
            match g.get(&key(ws, svc)) {
                Some(p) if p.alive && !p.stop_requested => Some((p.pid, p.port, p.infra, p.started_ms)),
                _ => None,
            }
        };
        let Some((pid, port, infra, started)) = info else { return false };
        if self.probe(pid, port, infra).open {
            return true;
        }
        // No port configured: a worker may never listen, so don't wait forever.
        port.is_none() && now_ms().saturating_sub(started) > NO_PORT_GRACE_MS
    }

    pub fn start_service(self: &Arc<Self>, ws: &Workspace, svc_in: &Service) -> Result<(), StartError> {
        let k = key(&ws.id, &svc_in.name);
        if self.is_alive(&ws.id, &svc_in.name) {
            return Ok(());
        }
        self.stop_proxy(&k);

        if let Some(port) = svc_in.port {
            if let Some(owner) = ports::owner(port) {
                let who = match &owner.container {
                    Some(c) => format!("Docker container `{c}`"),
                    None => format!("{} (PID {})", owner.process, owner.pid),
                };
                return Err(StartError {
                    message: format!("Port {port} is already in use by {who}"),
                    conflict: Some(Conflict { service: svc_in.name.clone(), port, pid: owner.pid, process: owner.process, container: owner.container }),
                });
            }
        }

        // Behind the fault proxy the service moves to a hidden port and the proxy takes over the public one.
        let moved: Service;
        let mut proxy: Option<(u16, u16)> = None;
        let svc: &Service = match (svc_in.chaos, svc_in.port) {
            (true, Some(public)) => {
                let internal = free_port().ok_or_else(|| "No free port available for the service behind the fault proxy".to_string())?;
                let mut s = svc_in.clone();
                s.port = Some(internal);
                if s.kind == "infra" {
                    s.container_port = Some(svc_in.container_port.unwrap_or(public));
                }
                moved = s;
                proxy = Some((public, internal));
                &moved
            }
            _ => svc_in,
        };

        let env = resolve_env(ws, svc, &ws.profile);
        let mut container: Option<String> = None;
        let mut effective = svc.command.clone();

        let mut cmd = if svc.kind == "infra" {
            let image = svc.image.clone().filter(|i| !i.trim().is_empty()).ok_or_else(|| "This infra service has no Docker image".to_string())?;
            let cname = format!("dw-{}-{}", sanitize_name(&ws.name).to_lowercase(), sanitize_name(&svc.name).to_lowercase());
            // Clear any leftover container with the same name from a previous run.
            let _ = crate::util::run("docker", &["rm", "-f", &cname], None);
            let mut c = command("docker");
            c.args(["run", "--rm", "--name", &cname]);
            if let Some(hp) = svc.port {
                c.arg("-p").arg(format!("{}:{}", hp, svc.container_port.unwrap_or(hp)));
            }
            if let Some(dp) = svc.data_path.as_ref().filter(|d| !d.trim().is_empty()) {
                c.arg("-v").arg(format!("{cname}-data:{dp}"));
            }
            for (k, v) in &env {
                if k != "PORT" {
                    c.arg("-e").arg(format!("{k}={v}"));
                }
            }
            c.arg(&image);
            if !svc.command.trim().is_empty() {
                c.args(svc.command.split_whitespace());
            }
            container = Some(cname);
            c
        } else {
            if svc.command.trim().is_empty() {
                return Err("No start command configured for this service".to_string().into());
            }
            if !std::path::Path::new(&svc.repo_path).is_dir() {
                return Err(format!("Working directory not found: {}", svc.repo_path).into());
            }
            // Make the configured port reach tools that ignore the PORT env var (Vite, Next, Angular...).
            effective = crate::portargs::apply(&svc.repo_path, &svc.command, svc.port).command;
            // The terminal chosen for this service (cmd / PowerShell / Bash).
            let mut c = crate::shell::build(&svc.shell, &effective, &crate::shell::Setup { no_profile: svc.no_profile, init_script: &svc.shell_init })?;
            c.current_dir(&svc.repo_path);
            for (k, v) in &env {
                c.env(k, v);
            }
            c
        };

        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let shown = if svc.kind == "infra" {
            format!("docker run {}", svc.image.clone().unwrap_or_default())
        } else if svc.shell.trim().is_empty() {
            effective.clone()
        } else {
            format!("{} [{}]", effective, svc.shell.trim())
        };
        let infra = svc.kind == "infra";
        if let Some((public, internal)) = proxy {
            let handle = ProxyHandle::start(public, internal, if infra { Mode::Tcp } else { Mode::Http }, self.faults_for(&k))?;
            self.proxies.lock().unwrap().insert(k.clone(), handle);
        }
        let mut child: Child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.stop_proxy(&k);
                return Err(format!("Failed to start `{shown}`: {e}").into());
            }
        };
        let pid = child.id();

        self.log(&ws.id, &svc.name, "system", &format!("▶ started `{shown}` (pid {pid}, profile {})", ws.profile));
        if let Some((public, internal)) = proxy {
            self.log(&ws.id, &svc.name, "system", &format!("⚡ fault proxy on :{public} → service on :{internal} (clients keep using :{public})"));
        }

        self.procs.lock().unwrap().insert(
            k.clone(),
            Proc {
                pid,
                started_ms: now_ms(),
                alive: true,
                stop_requested: false,
                exit_code: None,
                port: svc.port,
                container,
                announced: vec![],
                infra,
                public_port: proxy.map(|p| p.0),
            },
        );

        if let Some(out) = child.stdout.take() {
            self.spawn_reader(out, ws.id.clone(), svc.name.clone(), "stdout");
        }
        if let Some(err) = child.stderr.take() {
            self.spawn_reader(err, ws.id.clone(), svc.name.clone(), "stderr");
        }

        let rt = Arc::clone(self);
        let (wsid, name) = (ws.id.clone(), svc.name.clone());
        thread::spawn(move || {
            let status = child.wait();
            let code = status.ok().and_then(|s| s.code());
            // Drop the fault proxy first: a restart only proceeds once we mark the process dead, so the
            // new run's proxy can never be torn down by this (old) exit handler.
            rt.stop_proxy(&k);
            let mut requested = false;
            if let Some(p) = rt.procs.lock().unwrap().get_mut(&k) {
                p.alive = false;
                p.exit_code = code;
                requested = p.stop_requested;
            }
            let msg = if requested {
                "■ stopped".to_string()
            } else {
                format!("■ exited with code {}", code.map_or("?".to_string(), |c| c.to_string()))
            };
            rt.log(&wsid, &name, "system", &msg);
        });
        Ok(())
    }

    fn stop_proxy(&self, k: &str) {
        if let Some(h) = self.proxies.lock().unwrap().remove(k) {
            h.stop();
        }
    }

    fn faults_for(&self, k: &str) -> Arc<Mutex<Faults>> {
        self.faults.lock().unwrap().entry(k.to_string()).or_insert_with(|| Arc::new(Mutex::new(Faults::default()))).clone()
    }

    /// Replace the faults for a service. Applies immediately, even to a running proxy. Never persisted.
    pub fn chaos_set(&self, ws: &str, svc: &str, f: Faults) {
        *self.faults_for(&key(ws, svc)).lock().unwrap() = chaos::sanitize(f);
    }

    /// Clear faults for one service, or for the whole workspace.
    pub fn chaos_reset(&self, ws: &str, svc: Option<&str>) {
        match svc {
            Some(s) => *self.faults_for(&key(ws, s)).lock().unwrap() = Faults::default(),
            None => {
                let prefix = format!("{ws}/");
                for (k, f) in self.faults.lock().unwrap().iter() {
                    if k.starts_with(&prefix) {
                        *f.lock().unwrap() = Faults::default();
                    }
                }
            }
        }
    }

    pub fn chaos_states(&self, ws: &Workspace) -> Vec<ChaosState> {
        ws.services
            .iter()
            .map(|svc| {
                let k = key(&ws.id, &svc.name);
                let (active, upstream, stats) = match self.proxies.lock().unwrap().get(&k) {
                    Some(h) => (true, Some(h.upstream), Some(h.stats())),
                    None => (false, None, None),
                };
                ChaosState {
                    service: svc.name.clone(),
                    kind: svc.kind.clone(),
                    mode: if svc.kind == "infra" { Mode::Tcp } else { Mode::Http },
                    enabled: svc.chaos,
                    active,
                    public_port: svc.port,
                    upstream_port: upstream,
                    faults: self.faults_for(&k).lock().unwrap().clone(),
                    stats,
                }
            })
            .collect()
    }

    /// Fire one request at the service's public port (through the proxy when it is on).
    pub fn chaos_probe(&self, svc: &Service, path: &str) -> Result<ProbeResult, String> {
        let port = svc.port.ok_or("This service has no port to test")?;
        Ok(chaos::probe(port, path))
    }

    fn spawn_reader<R: Read + Send + 'static>(self: &Arc<Self>, r: R, ws: String, svc: String, stream: &'static str) {
        let rt = Arc::clone(self);
        thread::spawn(move || {
            let mut br = BufReader::new(r);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match br.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&buf);
                        rt.log(&ws, &svc, stream, text.trim_end_matches(['\n', '\r']));
                    }
                }
            }
        });
    }

    pub fn stop_service(&self, ws: &str, svc: &str) -> Result<(), String> {
        let (pid, container) = {
            let mut g = self.procs.lock().unwrap();
            match g.get_mut(&key(ws, svc)) {
                Some(p) if p.alive => {
                    p.stop_requested = true;
                    (p.pid, p.container.clone())
                }
                _ => return Ok(()),
            }
        };
        if let Some(c) = container {
            let _ = crate::util::run("docker", &["stop", "-t", "3", &c], None);
        }
        let _ = ports::kill_tree(pid);
        Ok(())
    }

    pub fn stop_all(&self, ws: &str) {
        let names: Vec<String> = {
            let g = self.procs.lock().unwrap();
            g.iter().filter(|(k, p)| p.alive && k.starts_with(&format!("{ws}/"))).map(|(k, _)| k[ws.len() + 1..].to_string()).collect()
        };
        for n in names {
            let _ = self.stop_service(ws, &n);
        }
    }

    pub fn stop_everything(&self) {
        let keys: Vec<(String, String)> = {
            let g = self.procs.lock().unwrap();
            g.iter()
                .filter(|(_, p)| p.alive)
                .filter_map(|(k, _)| k.split_once('/').map(|(a, b)| (a.to_string(), b.to_string())))
                .collect()
        };
        for (w, s) in keys {
            let _ = self.stop_service(&w, &s);
        }
    }

    pub fn status(&self, ws: &Workspace) -> Vec<ServiceRuntime> {
        // Resource usage: sum the whole process tree under each service's root pid.
        let mut usage: HashMap<u32, (f64, f32)> = HashMap::new();
        {
            let alive: Vec<u32> = {
                let g = self.procs.lock().unwrap();
                g.iter().filter(|(k, p)| p.alive && k.starts_with(&format!("{}/", ws.id))).map(|(_, p)| p.pid).collect()
            };
            if !alive.is_empty() {
                self.refresh_snapshot();
                let parents = self.snap.lock().unwrap().parents.clone();
                let sys = self.sys.lock().unwrap();
                for root in alive {
                    let (mut mem, mut cpu) = (0f64, 0f32);
                    for (pid, proc_) in sys.processes() {
                        if in_tree(&parents, pid.as_u32(), root) {
                            mem += proc_.memory() as f64 / 1024.0 / 1024.0;
                            cpu += proc_.cpu_usage();
                        }
                    }
                    usage.insert(root, (mem, cpu));
                }
            }
        }

        ws.services
            .iter()
            .map(|svc| {
                let snapshot = {
                    let g = self.procs.lock().unwrap();
                    g.get(&key(&ws.id, &svc.name)).map(|p| (p.pid, p.started_ms, p.alive, p.stop_requested, p.exit_code, p.port, p.infra, p.public_port))
                };
                let chaos_state = {
                    let k = key(&ws.id, &svc.name);
                    if self.proxies.lock().unwrap().contains_key(&k) {
                        if self.faults_for(&k).lock().unwrap().any() { "faulting" } else { "proxy" }
                    } else if svc.chaos {
                        "armed"
                    } else {
                        "off"
                    }
                };
                match snapshot {
                    None => ServiceRuntime {
                        name: svc.name.clone(),
                        status: "idle".into(),
                        pid: None,
                        started_ms: None,
                        exit_code: None,
                        mem_mb: None,
                        cpu: None,
                        port_open: svc.port.map_or(false, ports::is_open),
                        ports: vec![],
                        live_port: None,
                        port_mismatch: false,
                        health: "none".into(),
                        error_count: 0,
                        last_error: None,
                        health_hint: None,
                        chaos: chaos_state.into(),
                    },
                    Some((pid, started, alive, stopping, code, port, infra, public_port)) => {
                        let mut probe = if alive {
                            self.probe(pid, port, infra)
                        } else {
                            Probe { open: port.map_or(false, ports::is_open), mismatch: false, ports: vec![] }
                        };
                        // Behind the fault proxy the service is on a hidden port; report the public one.
                        if let Some(public) = public_port {
                            probe.mismatch = false;
                            probe.ports = if probe.open { vec![public] } else { vec![] };
                        }
                        let status = if alive {
                            if stopping {
                                "stopping"
                            } else if !probe.open && (port.is_some() || now_ms().saturating_sub(started) < NO_PORT_GRACE_MS) {
                                "starting"
                            } else {
                                "running"
                            }
                        } else if stopping || code == Some(0) {
                            "stopped"
                        } else {
                            "failed"
                        };
                        let live_port = match public_port.or(port) {
                            Some(p) if !probe.mismatch && probe.open => Some(p),
                            _ => probe.ports.first().copied(),
                        };
                        if alive && !stopping {
                            self.announce_ports(&ws.id, &svc.name, port, &probe);
                        }
                        let u = usage.get(&pid).copied();
                        let (error_count, last_error, health_hint) =
                            if alive && !stopping { self.error_summary(ws, svc, started) } else { (0, None, None) };
                        let health = match status {
                            "running" if error_count > 0 => "degraded",
                            "running" => "healthy",
                            "starting" => "starting",
                            _ => "none",
                        };
                        ServiceRuntime {
                            name: svc.name.clone(),
                            status: status.into(),
                            pid: if alive { Some(pid) } else { None },
                            started_ms: Some(started),
                            exit_code: code,
                            mem_mb: if alive { u.map(|x| x.0) } else { None },
                            cpu: if alive { u.map(|x| x.1) } else { None },
                            port_open: probe.open,
                            ports: probe.ports,
                            live_port,
                            port_mismatch: probe.mismatch,
                            health: health.into(),
                            error_count,
                            last_error,
                            health_hint,
                            chaos: chaos_state.into(),
                        }
                    }
                }
            })
            .collect()
    }

    /// Errors this service logged since it started and within the last minute:
    /// (distinct error count, last error text, explanation).
    fn error_summary(&self, ws: &Workspace, svc: &Service, started_ms: u64) -> (u32, Option<String>, Option<String>) {
        let cutoff = now_ms().saturating_sub(ERROR_WINDOW_MS).max(started_ms);
        let (mut count, mut last, mut refused) = (0u32, None::<String>, None::<u16>);
        {
            let logs = self.logs.lock().unwrap();
            for l in logs.iter().rev().take_while(|l| l.ts >= cutoff) {
                if l.workspace != ws.id || l.service != svc.name || l.stream == "system" || l.level != "error" {
                    continue;
                }
                if l.head {
                    count += 1;
                    if last.is_none() {
                        last = Some(l.text.clone());
                    }
                }
                if refused.is_none() {
                    refused = refused_port(&l.text);
                }
            }
        }
        // A refused connection seen only in continuation lines (its head scrolled out) is still an error.
        if count == 0 && refused.is_some() {
            count = 1;
        }
        if count == 0 {
            return (0, None, None);
        }
        let hint = refused.map(|p| match ws.services.iter().find(|o| o.name != svc.name && o.port == Some(p)) {
            Some(o) if !self.is_alive(&ws.id, &o.name) => {
                let listed = svc.depends_on.contains(&o.name);
                format!(
                    "Can't reach `{}` (:{p}): it isn't running.{}",
                    o.name,
                    if listed { "" } else { " It isn't in this service's “Starts after” list, so Up won't start it first." }
                )
            }
            Some(o) => format!("`{}` (:{p}) refused the connection: it is running but not accepting connections yet, or crashed.", o.name),
            None => format!("Connection refused on :{p}, and no service in this workspace uses that port."),
        });
        (count, last, hint)
    }

    /// Log (once per change) which ports a service is really listening on, and warn on a mismatch.
    fn announce_ports(&self, ws: &str, svc: &str, configured: Option<u16>, probe: &Probe) {
        if probe.ports.is_empty() {
            return;
        }
        {
            let mut g = self.procs.lock().unwrap();
            match g.get_mut(&key(ws, svc)) {
                Some(p) if p.announced != probe.ports => p.announced = probe.ports.clone(),
                _ => return,
            }
        }
        let list = probe.ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(", ");
        let msg = match configured {
            Some(c) if probe.mismatch => format!("⚠ expected :{c} but the service is listening on {list}. Add `--port {{port}}` to the start command, or change the port to match."),
            _ => format!("● listening on {list}"),
        };
        self.log(ws, svc, "system", &msg);
    }

    pub fn system_stats(&self) -> SystemStats {
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        SystemStats {
            cpu: sys.global_cpu_usage(),
            cores: sys.cpus().len() as u32,
            mem_used_gb: sys.used_memory() as f64 / 1024.0 / 1024.0 / 1024.0,
            mem_total_gb: sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0,
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    pub cpu: f32,
    pub cores: u32,
    pub mem_used_gb: f64,
    pub mem_total_gb: f64,
}

#[cfg(test)]
pub fn refused_port_for_test(text: &str) -> Option<u16> {
    refused_port(text)
}
