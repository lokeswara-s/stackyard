use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::git;
use crate::model::{EnvVar, Service, Settings, Workspace};
use crate::procs::Runtime;
use crate::up;
use crate::util::run;

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("dw-test-{}-{}", name, crate::util::now_ms()));
    fs::create_dir_all(&p).unwrap();
    p
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn g(dir: &PathBuf, args: &[&str]) {
    let mut full = vec!["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"];
    full.extend_from_slice(args);
    let o = run("git", &full, Some(dir.to_str().unwrap())).unwrap();
    assert!(o.ok, "git {:?} failed: {}", args, o.error_text());
}

#[test]
fn git_status_and_safe_pull() {
    let root = tmp("git");
    let remote = root.join("remote.git");
    let a = root.join("a");
    let b = root.join("b");
    fs::create_dir_all(&remote).unwrap();
    g(&remote, &["init", "--bare", "-b", "main"]);

    run("git", &["clone", remote.to_str().unwrap(), a.to_str().unwrap()], None).unwrap();
    fs::write(a.join("f.txt"), "one\n").unwrap();
    g(&a, &["add", "."]);
    g(&a, &["commit", "-m", "init"]);
    g(&a, &["push", "-u", "origin", "HEAD:main"]);

    run("git", &["clone", remote.to_str().unwrap(), b.to_str().unwrap()], None).unwrap();

    // A new commit lands on the remote from clone `a`.
    fs::write(a.join("f.txt"), "one\ntwo\n").unwrap();
    g(&a, &["commit", "-am", "second"]);
    g(&a, &["push"]);

    // Clone `b` is clean and behind: safe pull fast-forwards.
    let r = git::pull_safe(b.to_str().unwrap());
    assert_eq!(r.outcome, "synced", "{}", r.message);
    assert!(fs::read_to_string(b.join("f.txt")).unwrap().contains("two"));

    // Dirty tree + behind => skipped, and the file is untouched.
    fs::write(a.join("g.txt"), "x\n").unwrap();
    g(&a, &["add", "."]);
    g(&a, &["commit", "-m", "third"]);
    g(&a, &["push"]);
    fs::write(b.join("f.txt"), "local edit\n").unwrap();
    fs::write(b.join("new.txt"), "untracked\n").unwrap();
    let st = git::status(b.to_str().unwrap());
    assert!(st.is_repo);
    assert_eq!(st.branch, "main");
    assert_eq!(st.changes.len(), 1);
    assert_eq!(st.changes[0].path, "f.txt");
    assert_eq!(st.untracked, vec!["new.txt".to_string()]);
    let r = git::pull_safe(b.to_str().unwrap());
    assert_eq!(r.outcome, "skippedDirty", "{}", r.message);
    assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "local edit\n");
    let st = git::status(b.to_str().unwrap());
    assert_eq!(st.behind, 1);

    // Stash clears the tree, then pull works.
    git::stash(b.to_str().unwrap(), "my experiment", true).unwrap();
    let list = git::stash_list(b.to_str().unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].message.contains("my experiment"), "{}", list[0].message);
    assert!(!b.join("new.txt").exists());
    let r = git::pull_safe(b.to_str().unwrap());
    assert_eq!(r.outcome, "synced", "{}", r.message);

    // Branches: create one, see local/remote-only branches, switch with and without dirty changes.
    let bp = b.to_str().unwrap();
    g(&a, &["checkout", "-b", "feature-x"]);
    g(&a, &["push", "-u", "origin", "feature-x"]);
    git::fetch(bp).unwrap();
    let br = git::branches(bp).unwrap();
    assert_eq!(br.current, "main");
    assert!(br.local.iter().any(|x| x.name == "main" && x.current));
    assert!(br.remote.iter().any(|x| x.name == "origin/feature-x"), "{:?}", br.remote);
    assert!(!br.remote.iter().any(|x| x.name.ends_with("/main")), "main already exists locally");
    git::checkout(bp, "origin/feature-x", false, false).unwrap();
    assert_eq!(git::status(bp).branch, "feature-x");
    assert!(git::checkout(bp, "--evil", false, false).is_err());
    git::checkout(bp, "topic", true, false).unwrap();
    assert_eq!(git::status(bp).branch, "topic");
    // Stash-first switch back with a dirty tree.
    fs::write(b.join("f.txt"), "dirty again\n").unwrap();
    fs::write(b.join("scratch.txt"), "tmp\n").unwrap();
    let msg = git::checkout(bp, "main", false, true).unwrap();
    assert!(msg.contains("main") && msg.contains("stashed"), "{msg}");
    assert!(!b.join("scratch.txt").exists());
    let list = git::stash_list(bp).unwrap();
    assert_eq!(list.len(), 2);
    // Pop the older (named) stash by index, then drop the other.
    let idx = list.iter().find(|s| s.message.contains("my experiment")).unwrap().index;
    git::stash_pop(bp, Some(idx)).unwrap();
    assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap().trim(), "local edit");
    assert_eq!(git::stash_list(bp).unwrap().len(), 1);
    git::stash_drop(bp, 0).unwrap();
    assert!(git::stash_list(bp).unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn not_a_repo_is_reported() {
    let d = tmp("norepo");
    let st = git::status(d.to_str().unwrap());
    assert!(st.exists && !st.is_repo);
    let _ = fs::remove_dir_all(d);
}

#[test]
fn dependency_order_and_cycles() {
    let mk = |n: &str, deps: &[&str]| Service { name: n.into(), kind: "app".into(), depends_on: deps.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let o = up::order(&[mk("web", &["api"]), mk("api", &["db"]), mk("db", &[])]).unwrap();
    let names: Vec<&str> = o.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["db", "api", "web"]);
    assert!(up::order(&[mk("a", &["b"]), mk("b", &["a"])]).is_err());
}

fn node_server(dir: &PathBuf, banner: &str) {
    fs::write(
        dir.join("server.js"),
        format!(
            "console.log('{banner} booting with FOO=' + process.env.FOO);\nrequire('http').createServer((q,r)=>r.end('ok')).listen(process.env.PORT, ()=>console.log('\\x1b[32mINFO listening on ' + process.env.PORT + '\\x1b[0m'));\nsetInterval(()=>{{}}, 1000);\n"
        ),
    )
    .unwrap();
}

fn wait_for<F: Fn() -> bool>(secs: u64, f: F) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn up_pipeline_runs_services_in_order_with_env_and_logs() {
    let root = tmp("up");
    let (da, db) = (root.join("api"), root.join("web"));
    fs::create_dir_all(&da).unwrap();
    fs::create_dir_all(&db).unwrap();
    node_server(&da, "api");
    node_server(&db, "web");
    let (pa, pb) = (free_port(), free_port());

    let ws = Workspace {
        id: "t1".into(),
        name: "test".into(),
        profile: "local".into(),
        sync_before_up: true,
        env: vec![
            EnvVar { key: "FOO".into(), value: "global".into(), secret: false, profile: None },
            EnvVar { key: "FOO".into(), value: "prod-only".into(), secret: false, profile: Some("prod-sim".into()) },
        ],
        services: vec![
            Service { name: "web".into(), kind: "app".into(), repo_path: db.to_str().unwrap().into(), command: "node server.js".into(), port: Some(pb), depends_on: vec!["api".into()], ..Default::default() },
            Service { name: "api".into(), kind: "app".into(), repo_path: da.to_str().unwrap().into(), command: "node server.js".into(), port: Some(pa), ..Default::default() },
        ],
        ..Default::default()
    };

    let rt = Runtime::new();
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    assert!(wait_for(40, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")), "Up never finished");
    let run_ = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(run_.status, "success", "{:?}", run_);

    // Both ports are really serving.
    assert!(crate::ports::is_open(pa) && crate::ports::is_open(pb));

    // Runtime status + env resolved for the active profile (not the prod-sim one).
    let st = rt.status(&ws);
    assert!(st.iter().all(|s| s.status == "running" && s.pid.is_some()), "{:?}", st);
    let logs = rt.logs_after("t1", 0, 1000);
    assert!(logs.lines.iter().any(|l| l.service == "api" && l.text.contains("api booting with FOO=global")));
    // ANSI colour codes are stripped.
    assert!(logs.lines.iter().any(|l| l.text == format!("INFO listening on {pa}")), "{:?}", logs.lines.iter().map(|l| &l.text).collect::<Vec<_>>());
    // api (dependency) was started before web.
    let first = |svc: &str| logs.lines.iter().find(|l| l.service == svc && l.text.starts_with('▶')).unwrap().seq;
    assert!(first("api") < first("web"));

    // A second Up is idempotent (already-running services are reused).
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    assert!(wait_for(20, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")));
    assert_eq!(rt.up.lock().unwrap().as_ref().unwrap().status, "success");

    // Stop kills the whole process tree and frees the port.
    rt.stop_all("t1");
    assert!(wait_for(15, || !crate::ports::is_open(pa) && !crate::ports::is_open(pb)), "ports still open after stop");
    assert!(wait_for(10, || rt.status(&ws).iter().all(|s| s.status == "stopped")));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn up_reports_port_conflict() {
    let root = tmp("conflict");
    node_server(&root, "x");
    let squatter = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    let ws = Workspace {
        id: "t2".into(),
        name: "conflict".into(),
        services: vec![Service { name: "api".into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: "node server.js".into(), port: Some(port), ..Default::default() }],
        ..Default::default()
    };
    let rt = Runtime::new();
    up::start(rt.clone(), ws, Settings::default()).unwrap();
    assert!(wait_for(20, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")));
    let r = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(r.status, "failed");
    let c = r.conflict.expect("conflict reported");
    assert_eq!(c.port, port);
    assert_eq!(c.pid, std::process::id());
    // Nothing was started.
    assert!(!rt.is_alive("t2", "api"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn up_fails_when_service_crashes() {
    let root = tmp("crash");
    fs::write(root.join("boom.js"), "console.error('ERROR cannot connect to db'); process.exit(3);").unwrap();
    let ws = Workspace {
        id: "t3".into(),
        name: "crash".into(),
        services: vec![Service { name: "api".into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: "node boom.js".into(), port: Some(free_port()), ..Default::default() }],
        ..Default::default()
    };
    let rt = Runtime::new();
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    assert!(wait_for(20, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")));
    let r = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(r.status, "failed");
    assert!(r.error.unwrap().contains("cannot connect to db"));
    assert_eq!(rt.status(&ws)[0].status, "failed");
    let _ = fs::remove_dir_all(root);
}

fn docker_ready() -> bool {
    if !crate::docker::is_running() {
        eprintln!("SKIP: docker daemon not running");
        return false;
    }
    if !crate::docker::image_present("redis:alpine") {
        eprintln!("SKIP: redis:alpine not present locally");
        return false;
    }
    true
}

#[test]
fn docker_infra_lifecycle_and_conflict_attribution() {
    if !docker_ready() {
        return;
    }
    let port = free_port();
    let ws = Workspace {
        id: "tdock".into(),
        name: "dwtest-infra".into(),
        services: vec![Service { name: "cache".into(), kind: "infra".into(), image: Some("redis:alpine".into()), port: Some(port), container_port: Some(6379), ..Default::default() }],
        ..Default::default()
    };
    let cname = "dw-dwtest-infra-cache";
    let rt = Runtime::new();
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    assert!(wait_for(60, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")));
    let r = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(r.status, "success", "{:?}", r);
    let ps = run("docker", &["ps", "--filter", &format!("name={cname}"), "--format", "{{.Names}}"], None).unwrap();
    assert!(ps.stdout.contains(cname), "container not running: {}", ps.stdout);

    // The published port is attributed to the container, not just to Docker's engine process.
    let owner = crate::ports::owner(port).expect("port should be owned");
    assert_eq!(owner.container.as_deref(), Some(cname));

    // Killing the engine process is refused.
    let err = crate::ports::kill_pid(owner.pid).unwrap_err();
    assert!(err.contains("Docker"), "{err}");

    // A second workspace wanting the same port gets a conflict that names the container.
    let ws2 = Workspace { id: "tdock2".into(), name: "other".into(), services: vec![Service { name: "x".into(), kind: "app".into(), repo_path: std::env::temp_dir().to_str().unwrap().into(), command: "node -v".into(), port: Some(port), ..Default::default() }], ..Default::default() };
    up::start(rt.clone(), ws2, Settings::default()).unwrap();
    assert!(wait_for(30, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")));
    let r2 = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(r2.status, "failed");
    assert_eq!(r2.conflict.unwrap().container.as_deref(), Some(cname));

    // Stop removes the container (--rm) and frees the port.
    rt.stop_all("tdock");
    assert!(wait_for(30, || !crate::ports::is_open(port)), "port still open");
    let ps = run("docker", &["ps", "-a", "--filter", &format!("name={cname}"), "--format", "{{.Names}}"], None).unwrap();
    assert!(!ps.stdout.contains(cname), "container still exists: {}", ps.stdout);
}

#[test]
fn orphaned_managed_containers_are_cleaned_up() {
    if !docker_ready() {
        return;
    }
    let name = "dw-orphantest-leftover";
    let _ = run("docker", &["rm", "-f", name], None);
    let o = run("docker", &["run", "-d", "--rm", "--name", name, "redis:alpine"], None).unwrap();
    assert!(o.ok, "{}", o.error_text());
    assert!(crate::docker::cleanup_managed("dw-orphantest-") >= 1);
    let ps = run("docker", &["ps", "-a", "--filter", &format!("name={name}"), "--format", "{{.Names}}"], None).unwrap();
    assert!(!ps.stdout.contains(name));
    // unrelated containers are never matched by the prefix
    assert_eq!(crate::docker::cleanup_managed("dw-definitely-no-such-prefix-"), 0);
}

#[test]
fn env_precedence_is_independent_of_row_order() {
    let v = |k: &str, val: &str, p: Option<&str>| EnvVar { key: k.into(), value: val.into(), secret: false, profile: p.map(String::from) };
    let ws = Workspace {
        // profile-specific row listed BEFORE the all-profiles row
        env: vec![v("APP_ENV", "staging", Some("staging-like")), v("APP_ENV", "local", None), v("ONLY_PROD", "1", Some("prod-sim"))],
        ..Default::default()
    };
    let svc = Service { name: "s".into(), port: Some(4000), env: vec![v("APP_ENV", "svc-default", None)], ..Default::default() };
    let get = |profile: &str, key: &str| crate::model::resolve_env(&ws, &svc, profile).into_iter().find(|(k, _)| k == key).map(|(_, v)| v);
    // service-scoped beats global, even when the global one is profile-specific
    assert_eq!(get("staging-like", "APP_ENV").as_deref(), Some("svc-default"));
    assert_eq!(get("local", "ONLY_PROD"), None);
    assert_eq!(get("prod-sim", "ONLY_PROD").as_deref(), Some("1"));
    assert_eq!(get("local", "PORT").as_deref(), Some("4000"));

    // without a service override the profile-specific global must beat the all-profiles one
    let svc2 = Service { name: "s2".into(), ..Default::default() };
    let g2 = |profile: &str| crate::model::resolve_env(&ws, &svc2, profile).into_iter().find(|(k, _)| k == "APP_ENV").map(|(_, v)| v);
    assert_eq!(g2("staging-like").as_deref(), Some("staging"));
    assert_eq!(g2("local").as_deref(), Some("local"));
}

#[test]
fn detect_node_project() {
    let root = tmp("detect");
    fs::write(root.join("package.json"), r#"{"name":"@acme/storefront","scripts":{"dev":"next dev -p 3100"},"dependencies":{"next":"14"}}"#).unwrap();
    let d = crate::detect::detect(root.to_str().unwrap()).unwrap();
    assert_eq!(d.name, "storefront");
    assert_eq!(d.command, "npm run dev");
    assert_eq!(d.port, Some(3100));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn configured_port_reaches_tools_that_ignore_the_port_env() {
    let root = tmp("portargs");
    fs::write(
        root.join("package.json"),
        r#"{"scripts":{"dev":"vite","start":"node server.js","web":"next dev","both":"vite && node x.js","ng":"ng serve --open"},"devDependencies":{"vite":"7"}}"#,
    )
    .unwrap();
    let r = root.to_str().unwrap();
    let a = |cmd: &str, port: Option<u16>| crate::portargs::apply(r, cmd, port).command;

    // Vite ignores PORT: the flag must be appended (yarn/pnpm pass args through, npm needs `--`).
    assert_eq!(a("yarn run dev", Some(5173)), "yarn run dev --port 5173 --strictPort");
    assert_eq!(a("pnpm dev", Some(5173)), "pnpm dev --port 5173 --strictPort");
    assert_eq!(a("npm run dev", Some(5173)), "npm run dev -- --port 5173 --strictPort");
    assert_eq!(a("npm run web", Some(3100)), "npm run web -- -p 3100");
    assert_eq!(a("yarn run ng", Some(4300)), "yarn run ng --port 4300");
    assert_eq!(a("npx vite", Some(5000)), "npx vite --port 5000 --strictPort");
    assert_eq!(a("next dev", Some(3100)), "next dev -p 3100");

    // Stacks that read PORT from the environment are left alone, as are chained scripts and no-port services.
    assert_eq!(a("yarn run start", Some(3000)), "yarn run start");
    assert_eq!(a("npm start", Some(3000)), "npm start");
    assert_eq!(a("yarn run both", Some(3000)), "yarn run both");
    assert_eq!(a("yarn run dev", None), "yarn run dev");
    assert_eq!(a("node server.js", Some(3000)), "node server.js");

    // Explicit control always wins.
    assert_eq!(a("yarn run dev --port 9999", Some(5173)), "yarn run dev --port 9999");
    assert_eq!(a("vite --port={port}", Some(5173)), "vite --port=5173");
    assert_eq!(a("python -m http.server {port}", Some(8001)), "python -m http.server 8001");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn detects_the_real_listening_port_and_flags_a_mismatch() {
    let root = tmp("livedetect");
    // Ignores PORT entirely and binds an OS-chosen port on IPv6 loopback only (what `localhost` means in Node 18+).
    fs::write(
        root.join("server.js"),
        "const s=require('http').createServer((q,r)=>r.end('ok'));s.listen(0,'::1',()=>console.log('bound '+s.address().port));setInterval(()=>{},1000);\n",
    )
    .unwrap();
    let svc = |name: &str, port: Option<u16>| Service { name: name.into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: "node server.js".into(), port, ..Default::default() };
    let wrong = free_port();
    let ws = Workspace { id: "tlive".into(), name: "live".into(), services: vec![svc("auto", None), svc("mismatch", Some(wrong))], ..Default::default() };

    let rt = Runtime::new();
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    assert!(wait_for(40, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running")), "Up never finished");
    let run_ = rt.up.lock().unwrap().clone().unwrap();
    assert_eq!(run_.status, "success", "{:?}", run_);

    let st = rt.status(&ws);
    let auto = st.iter().find(|s| s.name == "auto").unwrap();
    assert!(!auto.ports.is_empty() && auto.live_port.is_some() && !auto.port_mismatch && auto.status == "running", "{:?}", auto);
    let mm = st.iter().find(|s| s.name == "mismatch").unwrap();
    assert!(mm.port_mismatch && mm.live_port.is_some() && mm.live_port != Some(wrong) && mm.status == "running", "{:?}", mm);

    let logs = rt.logs_after("tlive", 0, 1000);
    rt.stop_all("tlive");
    assert!(wait_for(10, || rt.status(&ws).iter().all(|s| s.status == "stopped")));
    let _ = fs::remove_dir_all(root);

    let texts: Vec<String> = logs.lines.iter().map(|l| format!("{}: {}", l.service, l.text)).collect();
    assert!(logs.lines.iter().any(|l| l.service == "auto" && l.text.contains("listening on :")), "{texts:?}");
    assert!(logs.lines.iter().any(|l| l.service == "mismatch" && l.text.contains(&format!("expected :{wrong}"))), "{texts:?}");
}

#[test]
fn real_vite_dev_server_honours_the_configured_port() {
    // Uses this repo's own Vite (skipped if dependencies are not installed).
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    if !repo.join("node_modules").join("vite").is_dir() {
        return;
    }
    let port = free_port();
    let svc = Service { name: "vite".into(), kind: "app".into(), repo_path: repo.to_str().unwrap().into(), command: "npm run dev".into(), port: Some(port), ..Default::default() };
    let ws = Workspace { id: "tvite".into(), name: "vite".into(), services: vec![svc], ..Default::default() };
    let rt = Runtime::new();
    up::start(rt.clone(), ws.clone(), Settings::default()).unwrap();
    let finished = wait_for(60, || rt.up.lock().unwrap().as_ref().map_or(false, |r| r.status != "running"));
    let status = rt.up.lock().unwrap().clone().map(|r| r.status);
    let st = rt.status(&ws);
    let logs: Vec<String> = rt.logs_after("tvite", 0, 200).lines.iter().map(|l| l.text.clone()).collect();
    rt.stop_all("tvite");
    wait_for(15, || rt.status(&ws).iter().all(|s| s.status != "running" && s.status != "starting"));
    assert!(finished && status.as_deref() == Some("success"), "{status:?} {logs:?}");
    assert_eq!(st[0].live_port, Some(port), "{:?} {logs:?}", st[0]);
    assert!(!st[0].port_mismatch, "{:?} {logs:?}", st[0]);
    assert!(logs.iter().any(|l| l.contains("npm run dev -- --port")), "{logs:?}");
}

#[test]
fn errors_in_logs_make_a_running_service_degraded_with_a_useful_hint() {
    let root = tmp("health");
    // Mimics a dev server whose proxy cannot reach its API: one error with a multi-line stack trace.
    fs::write(
        root.join("web.js"),
        "require('http').createServer((q,r)=>r.end('ok')).listen(process.env.PORT);\n\
         const api = process.env.API_PORT;\n\
         console.error('proxy error AggregateError [ECONNREFUSED]:');\n\
         console.error('    at internalConnectMultiple (node:net:1134:18)');\n\
         console.error('  [errors]: [');\n\
         console.error('    Error: connect ECONNREFUSED ::1:' + api);\n\
         console.error('        at afterConnectMultiple (node:net:1708:16) {');\n\
         console.error(\"      code: 'ECONNREFUSED',\");\n\
         console.error('    },');\n\
         setInterval(() => {}, 1000);\n",
    )
    .unwrap();
    node_server(&root, "ok");
    let (api_port, web_port, ok_port) = (free_port(), free_port(), free_port());
    let mk = |name: &str, cmd: &str, port: u16, env: Vec<EnvVar>, deps: Vec<String>| Service {
        name: name.into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: cmd.into(), port: Some(port), env, depends_on: deps, ..Default::default()
    };
    let ws = Workspace {
        id: "thealth".into(),
        name: "health".into(),
        services: vec![
            mk("api", "node server.js", api_port, vec![], vec![]),
            mk("web", "node web.js", web_port, vec![EnvVar { key: "API_PORT".into(), value: api_port.to_string(), secret: false, profile: None }], vec![]),
            mk("quiet", "node server.js", ok_port, vec![], vec![]),
        ],
        ..Default::default()
    };
    let rt = Runtime::new();
    // api is deliberately NOT started.
    for n in ["web", "quiet"] {
        let svc = ws.services.iter().find(|s| s.name == n).unwrap();
        rt.start_service(&ws, svc).map_err(|e| e.message).unwrap();
    }
    assert!(wait_for(20, || rt.is_ready("thealth", "web") && rt.is_ready("thealth", "quiet")));
    assert!(wait_for(10, || rt.status(&ws).iter().any(|s| s.name == "web" && s.error_count > 0)));
    let st = rt.status(&ws);
    let get = |n: &str| st.iter().find(|s| s.name == n).unwrap().clone();
    let (web, quiet, api) = (get("web"), get("quiet"), get("api"));
    let logs = rt.logs_after("thealth", 0, 500);
    rt.stop_all("thealth");
    wait_for(15, || rt.status(&ws).iter().all(|s| s.status != "running"));
    let _ = fs::remove_dir_all(root);

    // Still "running" (process up, port open) but flagged as degraded; the whole trace is one error.
    assert_eq!(web.status, "running");
    assert_eq!(web.health, "degraded", "{web:?}");
    assert_eq!(web.error_count, 1, "{web:?}");
    assert!(web.last_error.as_deref().unwrap_or("").starts_with("proxy error"), "{web:?}");
    let hint = web.health_hint.clone().unwrap_or_default();
    assert!(hint.contains("`api`") && hint.contains("isn't running") && hint.contains("Starts after"), "{hint}");
    // Stack-trace continuation lines are classified as errors too (they were plain info before).
    assert!(logs.lines.iter().filter(|l| l.service == "web" && l.stream == "stderr").all(|l| l.level == "error"));
    // A service with no errors stays healthy; one that isn't started has no health.
    assert_eq!((quiet.health.as_str(), quiet.error_count), ("healthy", 0), "{quiet:?}");
    assert_eq!(api.health, "none");
}

#[test]
fn refused_port_is_parsed_from_node_style_messages() {
    assert_eq!(crate::procs::refused_port_for_test("Error: connect ECONNREFUSED ::1:4000"), Some(4000));
    assert_eq!(crate::procs::refused_port_for_test("Error: connect ECONNREFUSED 127.0.0.1:4000"), Some(4000));
    assert_eq!(crate::procs::refused_port_for_test("AggregateError [ECONNREFUSED]:"), None);
    assert_eq!(crate::procs::refused_port_for_test("all good"), None);
}

// ---------- fault proxy ----------

use crate::chaos::{Faults, Mode, ProxyHandle};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// Send raw bytes to localhost:port and read until EOF or `timeout_ms` of silence.
fn raw_exchange(port: u16, req: &[u8], timeout_ms: u64) -> Result<(Option<u16>, String, u128), String> {
    let t0 = Instant::now();
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(Duration::from_millis(timeout_ms))).unwrap();
    s.write_all(req).map_err(|e| format!("write: {e}"))?;
    let mut data = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match s.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Err("timeout".into()),
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    if data.is_empty() {
        return Err("closed without a response".into());
    }
    let text = String::from_utf8_lossy(&data).to_string();
    let status = text.strip_prefix("HTTP/1.1 ").and_then(|r| r.get(..3)).and_then(|c| c.parse().ok());
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, body, t0.elapsed().as_millis()))
}

fn http_get(port: u16, path: &str, timeout_ms: u64) -> Result<(Option<u16>, String, u128), String> {
    raw_exchange(port, format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(), timeout_ms)
}

#[test]
fn fault_proxy_injects_faults_in_front_of_a_real_service() {
    let root = tmp("chaos");
    node_server(&root, "x");
    let public = free_port();
    let svc = Service { name: "api".into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: "node server.js".into(), port: Some(public), chaos: true, ..Default::default() };
    let ws = Workspace { id: "tchaos".into(), name: "chaos".into(), services: vec![svc.clone()], ..Default::default() };
    let rt = Runtime::new();
    let mut bad: Vec<String> = Vec::new();
    let mut check = |ok: bool, msg: String| {
        if !ok {
            bad.push(msg);
        }
    };

    rt.start_service(&ws, &svc).map_err(|e| e.message).unwrap();
    let ready = wait_for(20, || rt.is_ready("tchaos", "api"));
    check(ready, "service never became ready behind the proxy".into());

    // Clients keep using the public port; the service itself is hidden on another one.
    let r = http_get(public, "/", 3000);
    check(matches!(&r, Ok((Some(200), b, _)) if b == "ok"), format!("baseline through proxy: {r:?}"));
    let st = rt.status(&ws).remove(0);
    check(st.live_port == Some(public) && !st.port_mismatch && st.status == "running" && st.chaos == "proxy", format!("status: {st:?}"));
    let cs = rt.chaos_states(&ws).remove(0);
    check(cs.active && cs.upstream_port.is_some() && cs.upstream_port != Some(public), format!("state: {cs:?}"));

    // Latency
    rt.chaos_set("tchaos", "api", Faults { latency_ms: 300, ..Default::default() });
    let r = http_get(public, "/", 5000);
    check(matches!(&r, Ok((Some(200), _, ms)) if *ms >= 280), format!("latency: {r:?}"));
    check(rt.status(&ws)[0].chaos == "faulting", "status should say faulting".into());

    // Injected error with a custom body, the service never sees it
    rt.chaos_set("tchaos", "api", Faults { error_rate: 100, status: 503, body: "{\"x\":1}".into(), ..Default::default() });
    let r = http_get(public, "/", 3000);
    check(matches!(&r, Ok((Some(503), b, _)) if b.contains("\"x\":1")), format!("error injection: {r:?}"));

    // Path filter: only /pay is affected
    rt.chaos_set("tchaos", "api", Faults { error_rate: 100, status: 500, path_prefix: "/pay".into(), ..Default::default() });
    let (other, pay) = (http_get(public, "/other", 3000), http_get(public, "/pay/1", 3000));
    check(matches!(&other, Ok((Some(200), _, _))), format!("unfiltered path: {other:?}"));
    check(matches!(&pay, Ok((Some(500), _, _))), format!("filtered path: {pay:?}"));

    // Drop and blackhole
    rt.chaos_set("tchaos", "api", Faults { drop_rate: 100, ..Default::default() });
    let r = http_get(public, "/", 2000);
    check(r.is_err() && r.as_ref().unwrap_err() != "timeout", format!("drop should cut the connection: {r:?}"));
    rt.chaos_set("tchaos", "api", Faults { blackhole: true, ..Default::default() });
    let r = http_get(public, "/", 600);
    check(r.as_ref().err().map(|e| e.as_str()) == Some("timeout"), format!("blackhole should hang: {r:?}"));

    // Reset: back to normal, and the counters saw all of it
    rt.chaos_reset("tchaos", Some("api"));
    let r = http_get(public, "/", 3000);
    check(matches!(&r, Ok((Some(200), _, _))), format!("after reset: {r:?}"));
    let s = rt.chaos_states(&ws).remove(0).stats.unwrap();
    check(s.injected_errors >= 2 && s.dropped >= 1 && s.held >= 1 && s.forwarded >= 4 && s.added_latency_ms >= 280, format!("stats: {s:?}"));
    check(s.recent.iter().any(|e| e.outcome == "error" && e.status == Some(503)), "event feed missing the injected error".into());
    check(rt.status(&ws)[0].chaos == "proxy", "status should be back to proxy".into());

    // Restarting a proxied service must not lose or leak the proxy
    rt.stop_service("tchaos", "api").unwrap();
    check(wait_for(15, || !rt.is_alive("tchaos", "api") && !crate::ports::is_open(public)), "proxy port not released after stop".into());
    rt.start_service(&ws, &svc).map_err(|e| e.message).unwrap();
    check(wait_for(20, || rt.is_ready("tchaos", "api")), "not ready after restart".into());
    let r = http_get(public, "/", 3000);
    check(matches!(&r, Ok((Some(200), _, _))), format!("after restart: {r:?}"));

    rt.stop_all("tchaos");
    check(wait_for(15, || !crate::ports::is_open(public)), "public port still open after stop_all".into());
    let _ = fs::remove_dir_all(root);
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn fault_proxy_tcp_mode_delays_drops_and_hangs() {
    let echo = TcpListener::bind("127.0.0.1:0").unwrap();
    let up = echo.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in echo.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = s;
                let mut b = [0u8; 1024];
                while let Ok(n) = s.read(&mut b) {
                    if n == 0 || s.write_all(&b[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let public = free_port();
    let faults = Arc::new(Mutex::new(Faults::default()));
    let proxy = ProxyHandle::start(public, up, Mode::Tcp, faults.clone()).unwrap();
    let ping = |timeout_ms: u64| -> Result<(String, u128), String> {
        let t0 = Instant::now();
        let mut s = TcpStream::connect(("127.0.0.1", public)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_millis(timeout_ms))).unwrap();
        s.write_all(b"ping").map_err(|e| e.to_string())?;
        let mut b = [0u8; 4];
        match s.read(&mut b) {
            Ok(0) => Err("closed".into()),
            Ok(n) => Ok((String::from_utf8_lossy(&b[..n]).to_string(), t0.elapsed().as_millis())),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Err("timeout".into()),
            Err(e) => Err(e.to_string()),
        }
    };
    let base = ping(2000);
    *faults.lock().unwrap() = Faults { latency_ms: 250, ..Default::default() };
    let slow = ping(3000);
    *faults.lock().unwrap() = Faults { drop_rate: 100, ..Default::default() };
    let dropped = ping(1500);
    *faults.lock().unwrap() = Faults { blackhole: true, ..Default::default() };
    let held = ping(500);
    *faults.lock().unwrap() = Faults::default();
    let healed = ping(2000);
    let stats = proxy.stats();
    proxy.stop();

    assert!(matches!(&base, Ok((p, _)) if p == "ping"), "{base:?}");
    assert!(matches!(&slow, Ok((p, ms)) if p == "ping" && *ms >= 230), "{slow:?}");
    assert!(dropped.is_err() && dropped.as_ref().unwrap_err() != "timeout", "{dropped:?}");
    assert_eq!(held.as_ref().err().map(|e| e.as_str()), Some("timeout"), "{held:?}");
    assert!(matches!(&healed, Ok((p, _)) if p == "ping"), "{healed:?}");
    assert!(stats.dropped >= 1 && stats.held >= 1 && stats.added_latency_ms >= 230, "{stats:?}");
    assert!(wait_for(5, || !crate::ports::is_open(public)), "proxy port not released");
}

#[test]
fn fault_proxy_forwards_request_bodies_and_passes_websockets_through() {
    // A server that reports how many body bytes it received.
    let root = tmp("chaosbody");
    fs::write(root.join("body.js"), "require('http').createServer((q,r)=>{let n=0;q.on('data',d=>n+=d.length);q.on('end',()=>r.end('len='+n))}).listen(process.env.PORT);setInterval(()=>{},1000);\n").unwrap();
    let up = free_port();
    let svc = Service { name: "body".into(), kind: "app".into(), repo_path: root.to_str().unwrap().into(), command: "node body.js".into(), port: Some(up), ..Default::default() };
    let ws = Workspace { id: "tbody".into(), name: "body".into(), services: vec![svc.clone()], ..Default::default() };
    let rt = Runtime::new();
    rt.start_service(&ws, &svc).map_err(|e| e.message).unwrap();
    let ready = wait_for(20, || rt.is_ready("tbody", "body"));

    // A raw TCP echo server stands in for a WebSocket backend.
    let echo = TcpListener::bind("127.0.0.1:0").unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in echo.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = s;
                let mut b = [0u8; 4096];
                while let Ok(n) = s.read(&mut b) {
                    if n == 0 || s.write_all(&b[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });

    let (pub_body, pub_ws) = (free_port(), free_port());
    let faults = Arc::new(Mutex::new(Faults::default()));
    let p1 = ProxyHandle::start(pub_body, up, Mode::Http, faults.clone()).unwrap();
    let p2 = ProxyHandle::start(pub_ws, echo_port, Mode::Http, faults.clone()).unwrap();

    let payload = "a".repeat(20_000);
    let post = format!("POST /u HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{payload}", payload.len());
    let with_length = raw_exchange(pub_body, post.as_bytes(), 3000);
    let chunked = raw_exchange(pub_body, b"POST /u HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n", 3000);

    // Even a big body is consumed cleanly when the request is answered with an injected error.
    *faults.lock().unwrap() = Faults { error_rate: 100, status: 429, ..Default::default() };
    let rejected = raw_exchange(pub_body, post.as_bytes(), 3000);

    // Upgrade requests ignore faults and are piped both ways.
    let upgrade_req = b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
    let ws_echo = (|| -> Result<String, String> {
        let mut s = TcpStream::connect(("127.0.0.1", pub_ws)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        s.write_all(upgrade_req).map_err(|e| e.to_string())?;
        let mut b = vec![0u8; upgrade_req.len()];
        s.read_exact(&mut b).map_err(|e| e.to_string())?;
        s.write_all(b"hello").map_err(|e| e.to_string())?;
        let mut h = [0u8; 5];
        s.read_exact(&mut h).map_err(|e| e.to_string())?;
        Ok(format!("{}|{}", String::from_utf8_lossy(&b).starts_with("GET /ws"), String::from_utf8_lossy(&h)))
    })();

    p1.stop();
    p2.stop();
    rt.stop_all("tbody");
    let _ = fs::remove_dir_all(root);

    assert!(ready);
    assert!(matches!(&with_length, Ok((Some(200), b, _)) if b == "len=20000"), "{with_length:?}");
    assert!(matches!(&chunked, Ok((Some(200), b, _)) if b == "len=10"), "{chunked:?}");
    assert!(matches!(&rejected, Ok((Some(429), _, _))), "{rejected:?}");
    assert_eq!(ws_echo.as_deref(), Ok("true|hello"));
}

#[test]
fn every_installed_terminal_runs_commands_with_args_quotes_exit_codes_and_an_init_script() {
    use crate::shell::{build, Setup};
    let dir = tmp("shells");
    for s in crate::shell::available().into_iter().filter(|s| s.available) {
        let ps = s.id == "powershell" || s.id == "pwsh";
        let win_cmd = s.id == "cmd" || (s.id.is_empty() && cfg!(windows));
        // The init script defines something the start command then uses, like a custom function in a profile.
        let (file, init, body) = if ps {
            ("init.ps1", "function dwhello { 'hello from init' }\n$env:DW_INIT = 'yes'\n", "dwhello; Write-Output \"env=$env:DW_INIT DW_T=$env:DW_T\"; node -e \"process.exit(3)\"")
        } else if win_cmd {
            ("init.cmd", "@echo off\r\nset DW_INIT=yes\r\n", "echo hello from init & echo env=%DW_INIT% DW_T=%DW_T% && exit 3")
        } else {
            ("init.sh", "dwhello() { echo hello from init; }\nexport DW_INIT=yes\n", "dwhello; echo \"env=$DW_INIT DW_T=$DW_T\"; exit 3")
        };
        let script = dir.join(file);
        fs::write(&script, init).unwrap();
        let mut c = build(&s.id, body, &Setup { no_profile: true, init_script: script.to_str().unwrap() }).unwrap();
        c.current_dir(&dir).env("DW_T", "stackyard");
        let o = c.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(out.contains("hello from init") && out.contains("env=yes DW_T=stackyard"), "[{}] {out} / {}", s.id, String::from_utf8_lossy(&o.stderr));
        assert_eq!(o.status.code(), Some(3), "[{}] exit code", s.id);

        // Without an init script: plain command with quotes.
        let plain = if ps { "Write-Output \"q=$env:DW_T\"" } else if win_cmd { "echo q=%DW_T%" } else { "echo \"q=$DW_T\"" };
        let mut c = build(&s.id, plain, &Setup { no_profile: true, init_script: "" }).unwrap();
        c.current_dir(&dir).env("DW_T", "stackyard");
        assert!(String::from_utf8_lossy(&c.output().unwrap().stdout).contains("q=stackyard"), "[{}]", s.id);
    }
    // The profile is loaded unless asked otherwise.
    if let Some(ps) = crate::shell::available().into_iter().find(|s| s.id == "powershell" && s.available) {
        let args = |np: bool| build(&ps.id, "x", &Setup { no_profile: np, init_script: "" }).unwrap().get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert!(!args(false).contains(&"-NoProfile".to_string()));
        assert!(args(true).contains(&"-NoProfile".to_string()));
    }
    let plain = Setup::default();
    assert!(build("fish", "x", &plain).is_err());
    assert!(build("", "x", &Setup { no_profile: false, init_script: "Z:\\definitely\\missing.ps1" }).unwrap_err().contains("Init script not found"));
    let _ = fs::remove_dir_all(dir);
}
#[test]
fn storage_report_cleans_caches_and_runs_and_prunes_old_cache_entries() {
    use std::sync::Arc;
    let root = tmp("storage");
    let data = root.join("data");
    let ci_dir = data.join("ci");
    // A previous session left a big run behind: startup moves it aside and deletes it in the background.
    fs::create_dir_all(ci_dir.join("runs").join("run-old").join("ws")).unwrap();
    fs::write(ci_dir.join("runs").join("run-old").join("ws").join("blob.bin"), vec![1u8; 4096]).unwrap();
    let ci = Arc::new(crate::ci::engine::Ci::new(ci_dir.clone()));
    assert!(!ci_dir.join("runs").join("run-old").exists());
    let t0 = Instant::now();
    while fs::read_dir(&ci_dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("runs-old-")) {
        assert!(t0.elapsed() < Duration::from_secs(10), "old runs were not removed");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Caches: one fresh, one 30 days old.
    let fresh = ci_dir.join("cache").join("proj").join("fresh");
    let stale = ci_dir.join("cache").join("proj").join("stale");
    for d in [&fresh, &stale] {
        fs::create_dir_all(d).unwrap();
        fs::write(d.join("pkg.bin"), vec![7u8; 5000]).unwrap();
    }
    fs::create_dir_all(ci_dir.join("gh-cache")).unwrap();
    fs::write(ci_dir.join("gh-cache").join("abc.tgz"), vec![9u8; 3000]).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    fs::File::options().write(true).open(&stale).map(|f| f.set_modified(old)).ok();
    // Directories can't be opened for writing on Windows: age the directory through a handle with backup semantics.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let f = fs::OpenOptions::new().write(true).custom_flags(0x0200_0000).open(&stale).unwrap();
        f.set_modified(old).unwrap();
    }
    let freed = crate::storage::prune_caches(&ci_dir);
    assert!(freed >= 5000, "freed {freed}");
    assert!(!stale.exists() && fresh.exists() && ci_dir.join("gh-cache").join("abc.tgz").exists());

    let rep = crate::storage::report(&ci);
    let cache = rep.items.iter().find(|i| i.id == "ciCache").unwrap();
    assert!(cache.bytes.unwrap() >= 8000, "{:?}", cache.bytes);
    assert!(rep.disk_free_bytes.is_some());
    assert!(rep.data_bytes >= 8000);

    // An unknown folder inside runs/ belongs to no run: "Pipeline run files" removes it.
    fs::create_dir_all(ci_dir.join("runs").join("orphan")).unwrap();
    fs::write(ci_dir.join("runs").join("orphan").join("x"), "x").unwrap();
    let r = crate::storage::clean(&ci, &["ciRuns".into(), "ciCache".into()]).unwrap();
    assert!(r.freed_bytes >= 5000, "{:?}", r);
    assert!(!ci_dir.join("runs").join("orphan").exists());
    assert!(!ci_dir.join("cache").exists() && !ci_dir.join("gh-cache").exists());
    assert!(crate::storage::clean(&ci, &["nope".into()]).is_err());
    let _ = fs::remove_dir_all(root);
}
