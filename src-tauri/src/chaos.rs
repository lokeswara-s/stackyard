//! Fault-injection proxy.
//!
//! The proxy owns a service's *public* port and forwards to the real service, which was started on a hidden
//! internal port. Everything that already talks to `localhost:<port>` therefore goes through it, and we can
//! add latency, answer with errors, drop or hang connections and throttle bandwidth, live.
//!
//! * `Http` mode understands requests: faults are decided per request (so keep-alive does not hide them).
//!   Each forwarded request uses its own upstream connection (`Connection: close`). Upgrades such as
//!   WebSockets (Vite HMR) are passed through untouched.
//! * `Tcp` mode is for databases/brokers: faults apply per connection and per chunk.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::util::now_ms;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Http,
    Tcp,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Faults {
    /// Added to every request (HTTP) or client chunk (TCP).
    pub latency_ms: u32,
    /// Random extra/less latency in the range ±jitter.
    pub jitter_ms: u32,
    /// Percent of requests answered with `status` instead of reaching the service (HTTP only).
    pub error_rate: u8,
    pub status: u16,
    /// Response body for injected errors (empty = a small JSON document).
    pub body: String,
    /// Percent of requests (HTTP) / connections and client chunks (TCP) that are cut.
    pub drop_rate: u8,
    /// Accept the connection and never answer.
    pub blackhole: bool,
    /// Bandwidth cap in KB/s, 0 = unlimited.
    pub throttle_kbps: u32,
    /// Only apply to HTTP requests whose path starts with this (empty = everything).
    pub path_prefix: String,
}

impl Default for Faults {
    fn default() -> Self {
        Faults { latency_ms: 0, jitter_ms: 0, error_rate: 0, status: 503, body: String::new(), drop_rate: 0, blackhole: false, throttle_kbps: 0, path_prefix: String::new() }
    }
}

impl Faults {
    pub fn any(&self) -> bool {
        self.latency_ms > 0 || self.error_rate > 0 || self.drop_rate > 0 || self.blackhole || self.throttle_kbps > 0
    }
    fn sanitized(mut self) -> Faults {
        self.error_rate = self.error_rate.min(100);
        self.drop_rate = self.drop_rate.min(100);
        self.latency_ms = self.latency_ms.min(120_000);
        self.jitter_ms = self.jitter_ms.min(60_000);
        if !(100..=599).contains(&self.status) {
            self.status = 503;
        }
        self.body.truncate(4096);
        self
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChaosEvent {
    pub seq: u64,
    pub ts: u64,
    pub method: String,
    pub path: String,
    /// forwarded | error | dropped | held | upstream-down | passthrough | connection
    pub outcome: String,
    pub status: Option<u16>,
    /// Total time the proxy spent on it.
    pub ms: u64,
    /// Latency the proxy added on purpose.
    pub added_ms: u64,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChaosStats {
    pub requests: u64,
    pub forwarded: u64,
    pub injected_errors: u64,
    pub dropped: u64,
    pub held: u64,
    pub upstream_down: u64,
    pub added_latency_ms: u64,
    pub rps: f32,
    pub recent: Vec<ChaosEvent>,
}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    forwarded: AtomicU64,
    errors: AtomicU64,
    dropped: AtomicU64,
    held: AtomicU64,
    upstream_down: AtomicU64,
    added_ms: AtomicU64,
}

struct Shared {
    c: Counters,
    events: Mutex<VecDeque<ChaosEvent>>,
    times: Mutex<VecDeque<u64>>,
    seq: AtomicU64,
}

impl Shared {
    fn request(&self) {
        self.c.requests.fetch_add(1, Ordering::Relaxed);
        let now = now_ms();
        let mut t = self.times.lock().unwrap();
        t.push_back(now);
        while t.front().map_or(false, |x| now.saturating_sub(*x) > 10_000) {
            t.pop_front();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn event(&self, method: &str, path: &str, outcome: &str, status: Option<u16>, started: Instant, added_ms: u64) {
        let ev = ChaosEvent {
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            ts: now_ms(),
            method: method.to_string(),
            path: path.chars().take(160).collect(),
            outcome: outcome.to_string(),
            status,
            ms: started.elapsed().as_millis() as u64,
            added_ms,
        };
        let mut q = self.events.lock().unwrap();
        q.push_back(ev);
        while q.len() > 200 {
            q.pop_front();
        }
    }
}

struct Ctx {
    upstream: u16,
    mode: Mode,
    faults: Arc<Mutex<Faults>>,
    shared: Shared,
    stop: AtomicBool,
    prefer_v6: AtomicBool,
}

pub struct ProxyHandle {
    pub upstream: u16,
    ctx: Arc<Ctx>,
}

// ---------- small helpers ----------

static RNG: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn rnd() -> u64 {
    // splitmix64 over an atomic counter: good enough for fault dice, no extra crate.
    let mut z = RNG.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed).wrapping_add(now_ms());
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn roll(pct: u8) -> bool {
    pct > 0 && (rnd() % 100) < pct as u64
}

fn sleep_ms(ms: u64, stop: &AtomicBool) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20).min(end.saturating_duration_since(Instant::now())));
    }
}

fn pick_latency(f: &Faults) -> u64 {
    if f.latency_ms == 0 {
        return 0;
    }
    let j = f.jitter_ms as i64;
    let delta = if j > 0 { (rnd() % (2 * j as u64 + 1)) as i64 - j } else { 0 };
    (f.latency_ms as i64 + delta).max(0) as u64
}

fn throttle(f: &Faults, n: usize) {
    if f.throttle_kbps > 0 && n > 0 {
        thread::sleep(Duration::from_micros(n as u64 * 1_000_000 / (f.throttle_kbps as u64 * 1024)));
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn connect_upstream(ctx: &Ctx) -> Option<TcpStream> {
    let v4 = SocketAddr::from(([127, 0, 0, 1], ctx.upstream));
    let v6 = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], ctx.upstream));
    let order = if ctx.prefer_v6.load(Ordering::Relaxed) { [v6, v4] } else { [v4, v6] };
    for a in order {
        if let Ok(s) = TcpStream::connect_timeout(&a, Duration::from_millis(1000)) {
            let _ = s.set_nodelay(true);
            // Remember which family the service listens on (Vite binds ::1 on newer Node).
            ctx.prefer_v6.store(a == v6, Ordering::Relaxed);
            return Some(s);
        }
    }
    None
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        418 => "I'm a teapot",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Injected Fault",
    }
}

fn write_fault_response(c: &mut TcpStream, status: u16, body: &str) {
    let body = if body.trim().is_empty() {
        format!("{{\"error\":\"injected_fault\",\"status\":{status},\"source\":\"stackyard\"}}")
    } else {
        body.to_string()
    };
    let ctype = if body.trim_start().starts_with(['{', '[']) { "application/json" } else { "text/plain; charset=utf-8" };
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Stackyard-Fault: injected\r\n\r\n",
        reason(status),
        body.len()
    );
    let _ = c.write_all(head.as_bytes());
    let _ = c.write_all(body.as_bytes());
    let _ = c.flush();
}

// ---------- HTTP parsing ----------

struct Head {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    content_length: Option<usize>,
    chunked: bool,
    upgrade: bool,
}

/// Read until the end of the request head. Gives up on EOF, errors, an oversized head, `stop`, or `deadline`.
fn read_head(c: &mut TcpStream, buf: &mut Vec<u8>, stop: &AtomicBool, deadline: Instant) -> Option<usize> {
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(i) = find(buf, b"\r\n\r\n") {
            return Some(i + 4);
        }
        if buf.len() > 65_536 || stop.load(Ordering::Relaxed) || Instant::now() > deadline {
            return None;
        }
        match c.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => return None,
        }
    }
}

fn parse_head(raw: &[u8]) -> Option<Head> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let (method, target, version) = (first.next()?.to_string(), first.next()?.to_string(), first.next().unwrap_or("HTTP/1.1").to_string());
    let mut headers = Vec::new();
    for l in lines {
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_str());
    let content_length = get("content-length").and_then(|v| v.parse().ok());
    let chunked = get("transfer-encoding").map_or(false, |v| v.to_lowercase().contains("chunked"));
    let upgrade = get("upgrade").is_some() || method.eq_ignore_ascii_case("CONNECT");
    Some(Head { method, target, version, headers, content_length, chunked, upgrade })
}

/// Move the request body from the client to `sink` (or just discard it when `sink` is None).
fn relay_body(c: &mut TcpStream, mut sink: Option<&mut TcpStream>, leftover: Vec<u8>, head: &Head) -> std::io::Result<()> {
    let _ = c.set_read_timeout(Some(Duration::from_secs(30)));
    fn put(s: &mut Option<&mut TcpStream>, d: &[u8]) -> std::io::Result<()> {
        match s {
            Some(x) => x.write_all(d),
            None => Ok(()),
        }
    }
    let mut tmp = [0u8; 8192];
    if head.chunked {
        let mut tail: Vec<u8> = Vec::new();
        let mut chunk = leftover;
        loop {
            if !chunk.is_empty() {
                put(&mut sink, &chunk)?;
                tail.extend_from_slice(&chunk);
                if tail.len() > 8 {
                    tail.drain(..tail.len() - 8);
                }
                if find(&tail, b"0\r\n\r\n").is_some() {
                    return Ok(());
                }
            }
            let n = c.read(&mut tmp)?;
            if n == 0 {
                return Ok(());
            }
            chunk = tmp[..n].to_vec();
        }
    } else if let Some(mut remaining) = head.content_length {
        let take = leftover.len().min(remaining);
        put(&mut sink, &leftover[..take])?;
        remaining -= take;
        while remaining > 0 {
            let n = c.read(&mut tmp[..remaining.min(8192)])?;
            if n == 0 {
                break;
            }
            put(&mut sink, &tmp[..n])?;
            remaining -= n;
        }
    }
    Ok(())
}

fn status_of(first_bytes: &[u8]) -> Option<u16> {
    let s = std::str::from_utf8(&first_bytes[..first_bytes.len().min(16)]).ok()?;
    s.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()
}

// ---------- connection handlers ----------

/// Bidirectional raw copy until either side closes. Used for upgrades and as the TCP engine.
fn pipe(mut from: TcpStream, mut to: TcpStream, ctx: Arc<Ctx>, client_side: bool) {
    let _ = from.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf = [0u8; 16384];
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            break;
        }
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if ctx.mode == Mode::Tcp {
                    let f = ctx.faults.lock().unwrap().clone();
                    if client_side && f.any() {
                        if roll(f.drop_rate) {
                            ctx.shared.c.dropped.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        let d = pick_latency(&f);
                        ctx.shared.c.added_ms.fetch_add(d, Ordering::Relaxed);
                        sleep_ms(d, &ctx.stop);
                    }
                    throttle(&f, n);
                }
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Both);
    let _ = from.shutdown(Shutdown::Both);
}

fn hold(c: &mut TcpStream, ctx: &Ctx) {
    // Accept and say nothing, until the client gives up (or 10 minutes pass).
    let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
    let end = Instant::now() + Duration::from_secs(600);
    let mut b = [0u8; 4096];
    while Instant::now() < end && !ctx.stop.load(Ordering::Relaxed) {
        match c.read(&mut b) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
}

fn http_conn(mut c: TcpStream, ctx: Arc<Ctx>) {
    let _ = c.set_nodelay(true);
    let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    // Wait for the request; connections that stay silent for 2 minutes are closed.
    let Some(head_end) = read_head(&mut c, &mut buf, &ctx.stop, Instant::now() + Duration::from_secs(120)) else { return };
    let started = Instant::now();
    let Some(head) = parse_head(&buf[..head_end]) else { return };
    let leftover = buf[head_end..].to_vec();
    ctx.shared.request();
    let (method, path) = (head.method.clone(), head.target.clone());
    let f = ctx.faults.lock().unwrap().clone();
    let applies = f.any() && (f.path_prefix.is_empty() || path.starts_with(&f.path_prefix));

    // WebSockets etc.: transparent, no faults (Vite HMR must keep working).
    if head.upgrade {
        let Some(mut up) = connect_upstream(&ctx) else {
            ctx.shared.c.upstream_down.fetch_add(1, Ordering::Relaxed);
            write_fault_response(&mut c, 502, "{\"error\":\"service_unreachable\"}");
            ctx.shared.event(&method, &path, "upstream-down", Some(502), started, 0);
            return;
        };
        let _ = up.write_all(&buf);
        ctx.shared.event(&method, &path, "passthrough", None, started, 0);
        if let (Ok(c2), Ok(u2)) = (c.try_clone(), up.try_clone()) {
            let ctx2 = ctx.clone();
            thread::spawn(move || pipe(u2, c2, ctx2, false));
        }
        pipe(c, up, ctx, true);
        return;
    }

    let mut added = 0u64;
    if applies {
        if f.blackhole {
            ctx.shared.c.held.fetch_add(1, Ordering::Relaxed);
            ctx.shared.event(&method, &path, "held", None, started, 0);
            hold(&mut c, &ctx);
            return;
        }
        if roll(f.drop_rate) {
            ctx.shared.c.dropped.fetch_add(1, Ordering::Relaxed);
            ctx.shared.event(&method, &path, "dropped", None, started, 0);
            let _ = c.shutdown(Shutdown::Both);
            return;
        }
        added = pick_latency(&f);
        if added > 0 {
            ctx.shared.c.added_ms.fetch_add(added, Ordering::Relaxed);
            sleep_ms(added, &ctx.stop);
        }
        if roll(f.error_rate) {
            let _ = relay_body(&mut c, None, leftover, &head);
            write_fault_response(&mut c, f.status, &f.body);
            ctx.shared.c.errors.fetch_add(1, Ordering::Relaxed);
            ctx.shared.event(&method, &path, "error", Some(f.status), started, added);
            let _ = c.shutdown(Shutdown::Both);
            return;
        }
    }

    // Forward to the real service.
    let Some(mut up) = connect_upstream(&ctx) else {
        ctx.shared.c.upstream_down.fetch_add(1, Ordering::Relaxed);
        write_fault_response(&mut c, 502, "{\"error\":\"service_unreachable\",\"source\":\"stackyard\"}");
        ctx.shared.event(&method, &path, "upstream-down", Some(502), started, added);
        return;
    };
    let mut out = format!("{} {} {}\r\n", head.method, head.target, head.version);
    for (k, v) in &head.headers {
        let lk = k.to_ascii_lowercase();
        if lk != "connection" && lk != "keep-alive" && lk != "proxy-connection" {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    if up.write_all(out.as_bytes()).is_err() || relay_body(&mut c, Some(&mut up), leftover, &head).is_err() {
        return;
    }
    let _ = up.set_read_timeout(Some(Duration::from_secs(120)));
    let mut tmp = [0u8; 16384];
    let mut status = None;
    loop {
        match up.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if status.is_none() {
                    status = status_of(&tmp[..n]);
                }
                if c.write_all(&tmp[..n]).is_err() {
                    break;
                }
                if applies {
                    throttle(&f, n);
                }
            }
        }
    }
    ctx.shared.c.forwarded.fetch_add(1, Ordering::Relaxed);
    ctx.shared.event(&method, &path, "forwarded", status, started, added);
    let _ = c.shutdown(Shutdown::Both);
}

fn tcp_conn(mut c: TcpStream, ctx: Arc<Ctx>) {
    let _ = c.set_nodelay(true);
    let started = Instant::now();
    ctx.shared.request();
    let f = ctx.faults.lock().unwrap().clone();
    if f.any() {
        if f.blackhole {
            ctx.shared.c.held.fetch_add(1, Ordering::Relaxed);
            ctx.shared.event("TCP", "connection", "held", None, started, 0);
            hold(&mut c, &ctx);
            return;
        }
        if roll(f.drop_rate) {
            ctx.shared.c.dropped.fetch_add(1, Ordering::Relaxed);
            ctx.shared.event("TCP", "connection", "dropped", None, started, 0);
            let _ = c.shutdown(Shutdown::Both);
            return;
        }
        let d = pick_latency(&f);
        ctx.shared.c.added_ms.fetch_add(d, Ordering::Relaxed);
        sleep_ms(d, &ctx.stop);
    }
    let Some(up) = connect_upstream(&ctx) else {
        ctx.shared.c.upstream_down.fetch_add(1, Ordering::Relaxed);
        ctx.shared.event("TCP", "connection", "upstream-down", None, started, 0);
        let _ = c.shutdown(Shutdown::Both);
        return;
    };
    ctx.shared.c.forwarded.fetch_add(1, Ordering::Relaxed);
    ctx.shared.event("TCP", "connection", "connection", None, started, 0);
    let (Ok(c2), Ok(u2)) = (c.try_clone(), up.try_clone()) else { return };
    let ctx2 = ctx.clone();
    thread::spawn(move || pipe(u2, c2, ctx2, false));
    pipe(c, up, ctx, true);
}

fn accept_loop(l: TcpListener, ctx: Arc<Ctx>) {
    let _ = l.set_nonblocking(true);
    while !ctx.stop.load(Ordering::Relaxed) {
        match l.accept() {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                let ctx = ctx.clone();
                thread::spawn(move || match ctx.mode {
                    Mode::Http => http_conn(s, ctx),
                    Mode::Tcp => tcp_conn(s, ctx),
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(15)),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

impl ProxyHandle {
    /// Bind `public` on loopback (IPv4 and, when available, IPv6) and forward to `upstream`.
    pub fn start(public: u16, upstream: u16, mode: Mode, faults: Arc<Mutex<Faults>>) -> Result<ProxyHandle, String> {
        let bind = |addr: SocketAddr| -> Result<TcpListener, std::io::Error> {
            // The previous owner may still be tearing down (restart): give it a moment.
            let mut last = None;
            for _ in 0..40 {
                match TcpListener::bind(addr) {
                    Ok(l) => return Ok(l),
                    Err(e) => {
                        last = Some(e);
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            Err(last.unwrap())
        };
        let v4 = bind(SocketAddr::from(([127, 0, 0, 1], public))).map_err(|e| format!("Fault proxy could not take port {public}: {e}"))?;
        let v6 = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], public))).ok();
        let ctx = Arc::new(Ctx {
            upstream,
            mode,
            faults,
            shared: Shared { c: Counters::default(), events: Mutex::new(VecDeque::new()), times: Mutex::new(VecDeque::new()), seq: AtomicU64::new(0) },
            stop: AtomicBool::new(false),
            prefer_v6: AtomicBool::new(false),
        });
        let c1 = ctx.clone();
        thread::spawn(move || accept_loop(v4, c1));
        if let Some(l) = v6 {
            let c2 = ctx.clone();
            thread::spawn(move || accept_loop(l, c2));
        }
        Ok(ProxyHandle { upstream, ctx })
    }

    pub fn stop(&self) {
        self.ctx.stop.store(true, Ordering::SeqCst);
    }

    pub fn stats(&self) -> ChaosStats {
        let s = &self.ctx.shared;
        let now = now_ms();
        let recent_requests = s.times.lock().unwrap().iter().filter(|t| now.saturating_sub(**t) <= 5_000).count();
        let events: Vec<ChaosEvent> = {
            let q = s.events.lock().unwrap();
            q.iter().rev().take(40).cloned().collect::<Vec<_>>().into_iter().rev().collect()
        };
        ChaosStats {
            requests: s.c.requests.load(Ordering::Relaxed),
            forwarded: s.c.forwarded.load(Ordering::Relaxed),
            injected_errors: s.c.errors.load(Ordering::Relaxed),
            dropped: s.c.dropped.load(Ordering::Relaxed),
            held: s.c.held.load(Ordering::Relaxed),
            upstream_down: s.c.upstream_down.load(Ordering::Relaxed),
            added_latency_ms: s.c.added_ms.load(Ordering::Relaxed),
            rps: recent_requests as f32 / 5.0,
            recent: events,
        }
    }
}

pub fn sanitize(f: Faults) -> Faults {
    f.sanitized()
}

// ---------- probe ----------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub status: Option<u16>,
    pub ms: u64,
    pub error: Option<String>,
    pub body: String,
}

/// Send one `GET /` (or `path`) to localhost:`port` and report what came back, like a tiny curl.
pub fn probe(port: u16, path: &str) -> ProbeResult {
    let started = Instant::now();
    let done = |status: Option<u16>, error: Option<String>, body: String| ProbeResult { status, ms: started.elapsed().as_millis() as u64, error, body };
    let addrs = [SocketAddr::from(([127, 0, 0, 1], port)), SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port))];
    let Some(mut s) = addrs.iter().find_map(|a| TcpStream::connect_timeout(a, Duration::from_millis(1500)).ok()) else {
        return done(None, Some(format!("Connection refused on :{port}")), String::new());
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(8)));
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    if s.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\nAccept: */*\r\nUser-Agent: stackyard-probe\r\n\r\n").as_bytes()).is_err() {
        return done(None, Some("Connection closed while sending the request".into()), String::new());
    }
    let mut data = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match s.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&tmp[..n]);
                if data.len() > 64 * 1024 {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return done(status_of(&data), Some("Timed out waiting for a response (8s)".into()), String::new());
            }
            Err(e) => {
                return done(status_of(&data), Some(format!("Connection reset: {e}")), String::new());
            }
        }
    }
    if data.is_empty() {
        return done(None, Some("Connection closed with no response".into()), String::new());
    }
    let text = String::from_utf8_lossy(&data).to_string();
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.chars().take(400).collect()).unwrap_or_default();
    done(status_of(&data), None, body)
}
