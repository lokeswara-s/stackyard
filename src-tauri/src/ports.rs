use serde::Serialize;
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::util::run;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Listener {
    pub port: u16,
    pub pid: u32,
    pub process: String,
    /// Set when the port is published by a Docker container (the OS-level owner is Docker's engine).
    pub container: Option<String>,
}

/// Docker / WSL engine processes own every published container port. Killing them takes down *all* containers.
pub fn is_engine_process(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("docker") || n.contains("wslrelay") || n.contains("vmmem") || n.contains("vpnkit") || n.contains("wslhost")
}

/// host port -> container name for every running container that publishes a port.
fn container_ports() -> HashMap<u16, String> {
    let mut m = HashMap::new();
    if let Ok(o) = run("docker", &["ps", "--format", "{{.Names}}\t{{.Ports}}"], None) {
        if !o.ok {
            return m;
        }
        for line in o.stdout.lines() {
            let mut it = line.splitn(2, '\t');
            let (name, ports) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            for part in ports.split(", ") {
                if let Some((host, _)) = part.split_once("->") {
                    if let Some(p) = host.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                        m.entry(p).or_insert_with(|| name.to_string());
                    }
                }
            }
        }
    }
    m
}

fn attach_containers(list: &mut [Listener]) {
    if list.iter().any(|l| is_engine_process(&l.process)) {
        let map = container_ports();
        for l in list.iter_mut() {
            if is_engine_process(&l.process) {
                l.container = map.get(&l.port).cloned();
            }
        }
    }
}

/// True if something accepts TCP connections on localhost:`port`.
pub fn is_open(port: u16) -> bool {
    let addrs: [SocketAddr; 2] = [
        SocketAddr::from(([127, 0, 0, 1], port)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
    ];
    addrs.iter().any(|a| TcpStream::connect_timeout(a, Duration::from_millis(120)).is_ok())
}

#[cfg(windows)]
fn process_names() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(o) = run("tasklist", &["/FO", "CSV", "/NH"], None) {
        for line in o.stdout.lines() {
            let cols: Vec<&str> = line.trim().trim_matches('"').split("\",\"").collect();
            if cols.len() >= 2 {
                if let Ok(pid) = cols[1].parse::<u32>() {
                    m.insert(pid, cols[0].to_string());
                }
            }
        }
    }
    m
}

/// (port, pid) of every TCP listener, IPv4 and IPv6 (a bare `netstat -ano` lists both;
/// `-p TCP` would hide sockets bound to `[::1]`, which is where Vite/Node bind `localhost`).
#[cfg(windows)]
pub fn listening_pids() -> Vec<(u16, u32)> {
    let mut seen: Vec<(u16, u32)> = Vec::new();
    if let Ok(o) = run("netstat", &["-ano"], None) {
        for line in o.stdout.lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // Proto Local Foreign State PID  (UDP rows have no state column and are skipped)
            if cols.len() >= 5 && cols[3].eq_ignore_ascii_case("LISTENING") {
                let port = cols[1].rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
                let pid = cols[4].parse::<u32>().ok();
                if let (Some(port), Some(pid)) = (port, pid) {
                    if !seen.contains(&(port, pid)) {
                        seen.push((port, pid));
                    }
                }
            }
        }
    }
    seen
}

#[cfg(not(windows))]
pub fn listening_pids() -> Vec<(u16, u32)> {
    let mut v: Vec<(u16, u32)> = Vec::new();
    if let Ok(o) = run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"], None) {
        let mut pid = 0u32;
        for line in o.stdout.lines() {
            if line.len() < 2 {
                continue;
            }
            let (tag, r) = line.split_at(1);
            match tag {
                "p" => pid = r.parse().unwrap_or(0),
                "n" => {
                    if let Some(port) = r.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                        if !v.contains(&(port, pid)) {
                            v.push((port, pid));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    v
}

#[cfg(windows)]
pub fn listeners() -> Vec<Listener> {
    let names = process_names();
    let mut v: Vec<Listener> = listening_pids()
        .into_iter()
        .map(|(port, pid)| Listener { port, pid, process: names.get(&pid).cloned().unwrap_or_else(|| "unknown".into()), container: None })
        .collect();
    v.sort_by_key(|l| l.port);
    attach_containers(&mut v);
    v
}

#[cfg(not(windows))]
pub fn listeners() -> Vec<Listener> {
    let mut v = Vec::new();
    if let Ok(o) = run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"], None) {
        let (mut pid, mut name) = (0u32, String::new());
        for line in o.stdout.lines() {
            match line.split_at(1) {
                ("p", r) => pid = r.parse().unwrap_or(0),
                ("c", r) => name = r.to_string(),
                ("n", r) => {
                    if let Some(port) = r.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                        v.push(Listener { port, pid, process: name.clone(), container: None });
                    }
                }
                _ => {}
            }
        }
    }
    v.sort_by_key(|l| l.port);
    v.dedup_by_key(|l| (l.port, l.pid));
    attach_containers(&mut v);
    v
}

pub fn owner(port: u16) -> Option<Listener> {
    if !is_open(port) {
        return None;
    }
    listeners().into_iter().find(|l| l.port == port)
}

pub fn kill_pid(pid: u32) -> Result<(), String> {
    if pid <= 4 || pid == std::process::id() {
        return Err("Refusing to kill a protected process".into());
    }
    if let Some(l) = listeners().into_iter().find(|l| l.pid == pid) {
        if is_engine_process(&l.process) {
            return Err(match l.container {
                Some(c) => format!("Port is published by Docker container `{c}`. Stop that container instead of killing Docker's engine process."),
                None => format!("`{}` is part of the Docker/WSL engine. Killing it would stop all your containers.", l.process),
            });
        }
    }
    kill_tree(pid)
}

#[cfg(windows)]
pub fn kill_tree(pid: u32) -> Result<(), String> {
    let o = run("taskkill", &["/PID", &pid.to_string(), "/T", "/F"], None)?;
    if o.ok { Ok(()) } else { Err(o.error_text()) }
}

#[cfg(not(windows))]
pub fn kill_tree(pid: u32) -> Result<(), String> {
    // Children are started in their own process group (see procs.rs).
    let group = format!("-{pid}");
    let o = run("kill", &["-TERM", "--", &group], None)?;
    if o.ok {
        return Ok(());
    }
    let o = run("kill", &["-TERM", &pid.to_string()], None)?;
    if o.ok { Ok(()) } else { Err(o.error_text()) }
}
