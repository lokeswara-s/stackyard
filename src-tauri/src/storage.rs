//! What Stackyard keeps on disk (pipeline checkouts, artifacts, caches) and in Docker, with ways to wipe it.
//! Workspaces and settings (`store.json`) are never touched here.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::ci::engine::Ci;
use crate::util::run;

/// Pipelines refuse to start with less free space than this (a full disk makes every job fail halfway).
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Pipeline caches older than this, or beyond the size cap, are pruned automatically at startup.
const CACHE_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);
const CACHE_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StorageItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub bytes: Option<u64>,
    /// Shown instead of `bytes` (Docker reports sizes as text).
    pub size_text: Option<String>,
    pub count: Option<u32>,
    /// Needs an explicit confirmation: it deletes data you may want (e.g. database volumes).
    pub danger: bool,
    /// Affects Docker as a whole, not only what Stackyard created.
    pub docker_wide: bool,
    /// Part of "Clean Stackyard files".
    pub routine: bool,
    /// Why it can't be cleaned right now.
    pub blocked: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StorageReport {
    pub data_dir: String,
    pub data_bytes: u64,
    pub disk_free_bytes: Option<u64>,
    pub min_free_bytes: u64,
    pub docker: bool,
    pub items: Vec<StorageItem>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanResult {
    pub freed_bytes: u64,
    pub messages: Vec<String>,
}

// ---------------------------------------------------------------- disk helpers

pub fn dir_size(p: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_symlink() {
                continue;
            }
            if t.is_dir() {
                stack.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn path_size(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => dir_size(p),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn remove_path(p: &Path) {
    if std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false) {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

/// Free space on the disk holding `p`.
pub fn free_bytes(p: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let norm = |x: &Path| x.to_string_lossy().trim_start_matches(r"\\?\").to_lowercase();
    let target = norm(&p);
    disks
        .list()
        .iter()
        .filter(|d| target.starts_with(&norm(d.mount_point())))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The cache entries of the pipeline simulator: GitLab `cache/<project>/<key>` and GitHub `gh-cache/<file>`.
fn cache_entries(ci_dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Ok(rd) = std::fs::read_dir(ci_dir.join("cache")) {
        for proj in rd.flatten() {
            if let Ok(rd2) = std::fs::read_dir(proj.path()) {
                v.extend(rd2.flatten().map(|e| e.path()));
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(ci_dir.join("gh-cache")) {
        v.extend(rd.flatten().map(|e| e.path()));
    }
    v
}

/// Drop stale pipeline caches and keep the rest under a size cap. Runs at startup so the folder cannot grow forever.
pub fn prune_caches(ci_dir: &Path) -> u64 {
    let now = SystemTime::now();
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = cache_entries(ci_dir).into_iter().map(|p| (mtime(&p), p)).map(|(t, p)| { let s = path_size(&p); (p, t, s) }).collect();
    let mut freed = 0;
    entries.retain(|(p, t, s)| {
        if now.duration_since(*t).map(|a| a > CACHE_MAX_AGE).unwrap_or(false) {
            remove_path(p);
            freed += s;
            false
        } else {
            true
        }
    });
    entries.sort_by_key(|(_, t, _)| *t);
    let mut total: u64 = entries.iter().map(|e| e.2).sum();
    for (p, _, s) in entries {
        if total <= CACHE_MAX_BYTES {
            break;
        }
        remove_path(&p);
        total -= s;
        freed += s;
    }
    freed
}

// ---------------------------------------------------------------- docker helpers

fn docker_lines(args: &[&str]) -> Vec<String> {
    run("docker", args, None).ok().filter(|o| o.ok).map(|o| o.stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

/// `dw-` containers (infra services) that are not running.
fn stopped_service_containers() -> Vec<String> {
    docker_lines(&["ps", "-a", "--filter", "name=dw-", "--format", "{{.ID}}\t{{.Names}}\t{{.State}}"])
        .into_iter()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let (id, name, state) = (it.next()?, it.next()?, it.next()?);
            (name.starts_with("dw-") && state != "running").then(|| id.to_string())
        })
        .collect()
}

fn pipeline_containers() -> Vec<String> {
    docker_lines(&["ps", "-aq", "--filter", "name=dwci-"])
}

/// Unused `dw-` volumes: the data of infra services (databases...) that no container uses right now.
fn service_volumes() -> Vec<String> {
    docker_lines(&["volume", "ls", "-q", "--filter", "dangling=true", "--filter", "name=dw-"]).into_iter().filter(|n| n.starts_with("dw-")).collect()
}

/// `docker system df` as (type -> (size, reclaimable)).
fn docker_df() -> Vec<(String, String, String)> {
    docker_lines(&["system", "df", "--format", "{{json .}}"])
        .into_iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(&l).ok()?;
            let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            Some((g("Type"), g("Size"), g("Reclaimable")))
        })
        .collect()
}

// ---------------------------------------------------------------- report

pub fn report(ci: &Ci) -> StorageReport {
    let ci_dir = ci.dir.clone();
    let data_dir = ci_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| ci_dir.clone());
    let running = ci.any_running();
    let blocked = running.then(|| "A pipeline is running. Wait for it to finish or cancel it.".to_string());
    let docker = crate::docker::is_running();

    let runs_bytes = dir_size(&ci_dir.join("runs"));
    let cache_bytes: u64 = cache_entries(&ci_dir).iter().map(|p| path_size(p)).sum();

    let mut items = vec![
        StorageItem {
            id: "ciRuns".into(),
            label: "Pipeline run files".into(),
            description: "Checkouts, artifacts and workspaces of finished pipeline runs. Their logs and results disappear from the Pipelines page.".into(),
            bytes: Some(runs_bytes),
            routine: true,
            blocked: blocked.clone(),
            ..Default::default()
        },
        StorageItem {
            id: "ciCache".into(),
            label: "Pipeline caches".into(),
            description: "GitLab `cache:` and GitHub `actions/cache` data (dependencies). Rebuilt on the next run; pruned automatically after 14 days or above 5 GB.".into(),
            bytes: Some(cache_bytes),
            routine: true,
            blocked: blocked.clone(),
            ..Default::default()
        },
    ];

    if docker {
        let pc = pipeline_containers().len() as u32;
        items.push(StorageItem {
            id: "ciContainers".into(),
            label: "Leftover pipeline containers & networks".into(),
            description: "Job containers (`dwci-*`) that a crashed or cancelled run left behind.".into(),
            count: Some(pc),
            routine: true,
            blocked: blocked.clone(),
            ..Default::default()
        });
        let sc = stopped_service_containers().len() as u32;
        items.push(StorageItem {
            id: "stoppedServices".into(),
            label: "Stopped service containers".into(),
            description: "Stopped containers (`dw-*`) of infra services. Their data volumes are kept.".into(),
            count: Some(sc),
            routine: true,
            ..Default::default()
        });
        let vols = service_volumes().len() as u32;
        items.push(StorageItem {
            id: "serviceData".into(),
            label: "Unused service data volumes".into(),
            description: "Data of infra services (databases, queues) that are not running. Deleting it resets them to empty.".into(),
            count: Some(vols),
            danger: true,
            ..Default::default()
        });
        let df = docker_df();
        let find = |t: &str| df.iter().find(|(ty, _, _)| ty.eq_ignore_ascii_case(t)).cloned();
        if let Some((_, size, rec)) = find("Build Cache") {
            items.push(StorageItem {
                id: "dockerBuildCache".into(),
                label: "Docker build cache".into(),
                description: "Layers cached by `docker build` for all your projects. Builds are slower the first time after clearing.".into(),
                size_text: Some(format!("{size} ({rec} reclaimable)")),
                docker_wide: true,
                ..Default::default()
            });
        }
        if let Some((_, size, rec)) = find("Images") {
            items.push(StorageItem {
                id: "dockerDanglingImages".into(),
                label: "Docker dangling images".into(),
                description: "Untagged leftovers of image builds and updates. Tagged images (like your runner images) are kept.".into(),
                size_text: Some(format!("all images {size}, {rec} reclaimable")),
                docker_wide: true,
                ..Default::default()
            });
        }
    }

    StorageReport {
        data_bytes: dir_size(&data_dir),
        disk_free_bytes: free_bytes(&data_dir),
        min_free_bytes: MIN_FREE_BYTES,
        data_dir: data_dir.to_string_lossy().into_owned(),
        docker,
        items,
    }
}

// ---------------------------------------------------------------- cleaning

fn parse_reclaimed(out: &str) -> Option<String> {
    out.lines().find_map(|l| l.trim().strip_prefix("Total reclaimed space:").map(|s| s.trim().to_string()))
}

pub fn clean(ci: &Ci, ids: &[String]) -> Result<CleanResult, String> {
    let mut res = CleanResult::default();
    let ci_dir = ci.dir.clone();
    let data_dir = ci_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| ci_dir.clone());
    let before = dir_size(&data_dir);
    for id in ids {
        match id.as_str() {
            "ciRuns" | "ciCache" | "ciContainers" if ci.any_running() => {
                return Err("A pipeline is running. Wait for it to finish or cancel it first.".into());
            }
            "ciRuns" => {
                let n = ci.purge_finished();
                // Anything left in runs/ belongs to no run we know of.
                let known: Vec<String> = ci.run_ids();
                if let Ok(rd) = std::fs::read_dir(ci_dir.join("runs")) {
                    for e in rd.flatten() {
                        if !known.contains(&e.file_name().to_string_lossy().into_owned()) {
                            remove_path(&e.path());
                        }
                    }
                }
                res.messages.push(format!("Removed the files of {n} pipeline run(s)"));
            }
            "ciCache" => {
                for p in cache_entries(&ci_dir) {
                    remove_path(&p);
                }
                let _ = std::fs::remove_dir_all(ci_dir.join("cache"));
                let _ = std::fs::remove_dir_all(ci_dir.join("gh-cache"));
                res.messages.push("Cleared pipeline caches".into());
            }
            "ciContainers" => {
                let n = pipeline_containers().len();
                crate::ci::exec::cleanup_leftovers();
                res.messages.push(format!("Removed {n} pipeline container(s) and their networks"));
            }
            "stoppedServices" => {
                let ids = stopped_service_containers();
                if !ids.is_empty() {
                    let mut args: Vec<&str> = vec!["rm", "-f"];
                    args.extend(ids.iter().map(|s| s.as_str()));
                    let _ = run("docker", &args, None);
                }
                res.messages.push(format!("Removed {} stopped service container(s)", ids.len()));
            }
            "serviceData" => {
                let vols = service_volumes();
                let mut gone = 0;
                for v in &vols {
                    if run("docker", &["volume", "rm", v], None).map(|o| o.ok).unwrap_or(false) {
                        gone += 1;
                    }
                }
                res.messages.push(format!("Deleted {gone} service data volume(s)"));
            }
            "dockerBuildCache" => {
                let o = run("docker", &["builder", "prune", "-af"], None)?;
                if !o.ok {
                    return Err(o.error_text());
                }
                res.messages.push(format!("Docker build cache cleared{}", parse_reclaimed(&o.stdout).map(|s| format!(" ({s})")).unwrap_or_default()));
            }
            "dockerDanglingImages" => {
                let o = run("docker", &["image", "prune", "-f"], None)?;
                if !o.ok {
                    return Err(o.error_text());
                }
                res.messages.push(format!("Dangling Docker images removed{}", parse_reclaimed(&o.stdout).map(|s| format!(" ({s})")).unwrap_or_default()));
            }
            other => return Err(format!("Unknown item `{other}`")),
        }
    }
    res.freed_bytes = before.saturating_sub(dir_size(&data_dir));
    Ok(res)
}
