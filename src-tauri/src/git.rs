use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

use crate::util::{command, Out};

fn git(path: &str, args: &[&str]) -> Result<Out, String> {
    let mut c = command("git");
    c.arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    let o = c.output().map_err(|e| format!("could not run git: {e}"))?;
    Ok(Out {
        ok: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    /// Single-letter status of the index (staged) side, '.' if none.
    pub index: String,
    /// Single-letter status of the work-tree side, '.' if none.
    pub worktree: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub path: String,
    pub exists: bool,
    pub is_repo: bool,
    pub error: Option<String>,
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: Vec<FileChange>,
    pub untracked: Vec<String>,
    pub last_commit: String,
    pub last_fetch_ms: Option<u64>,
    pub remote: Option<String>,
}

pub fn status(path: &str) -> GitStatus {
    let mut s = GitStatus { path: path.to_string(), ..Default::default() };
    if path.trim().is_empty() || !Path::new(path).is_dir() {
        s.error = Some("Folder not found".into());
        return s;
    }
    s.exists = true;
    match git(path, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(o) if o.ok && o.stdout.trim() == "true" => s.is_repo = true,
        Ok(_) => {
            s.error = Some("Not a git repository".into());
            return s;
        }
        Err(e) => {
            s.error = Some(e);
            return s;
        }
    }

    match git(path, &["status", "--porcelain=v2", "--branch", "--untracked-files=normal"]) {
        Ok(o) if o.ok => {
            for line in o.stdout.lines() {
                if let Some(rest) = line.strip_prefix("# branch.head ") {
                    s.branch = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
                    s.upstream = Some(rest.to_string());
                } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
                    for part in rest.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            s.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            s.behind = n.parse().unwrap_or(0);
                        }
                    }
                } else if line.starts_with("1 ") || line.starts_with("2 ") {
                    let renamed = line.starts_with("2 ");
                    let fields: Vec<&str> = line.splitn(if renamed { 10 } else { 9 }, ' ').collect();
                    if fields.len() >= 2 {
                        let xy: Vec<char> = fields[1].chars().collect();
                        let raw_path = fields.last().copied().unwrap_or("");
                        let path_part = if renamed { raw_path.split('\t').next().unwrap_or(raw_path) } else { raw_path };
                        s.changes.push(FileChange {
                            path: path_part.to_string(),
                            index: xy.first().copied().unwrap_or('.').to_string(),
                            worktree: xy.get(1).copied().unwrap_or('.').to_string(),
                            added: None,
                            removed: None,
                        });
                    }
                } else if line.starts_with("u ") {
                    let p = line.splitn(11, ' ').last().unwrap_or("").to_string();
                    s.changes.push(FileChange { path: p, index: "U".into(), worktree: "U".into(), added: None, removed: None });
                } else if let Some(p) = line.strip_prefix("? ") {
                    s.untracked.push(p.to_string());
                }
            }
        }
        Ok(o) => s.error = Some(o.error_text()),
        Err(e) => s.error = Some(e),
    }

    if let Ok(o) = git(path, &["diff", "--numstat", "HEAD"]) {
        if o.ok {
            let mut map: HashMap<String, (Option<u32>, Option<u32>)> = HashMap::new();
            for l in o.stdout.lines() {
                let mut it = l.splitn(3, '\t');
                let (a, r, p) = (it.next(), it.next(), it.next());
                if let (Some(a), Some(r), Some(p)) = (a, r, p) {
                    map.insert(p.to_string(), (a.parse().ok(), r.parse().ok()));
                }
            }
            for c in s.changes.iter_mut() {
                if let Some((a, r)) = map.get(&c.path) {
                    c.added = *a;
                    c.removed = *r;
                }
            }
        }
    }

    if let Ok(o) = git(path, &["log", "-1", "--format=%h %s"]) {
        if o.ok {
            s.last_commit = o.stdout.trim().to_string();
        }
    }
    if let Ok(o) = git(path, &["remote", "get-url", "origin"]) {
        if o.ok {
            s.remote = Some(o.stdout.trim().to_string());
        }
    }
    if let Ok(o) = git(path, &["rev-parse", "--git-dir"]) {
        if o.ok {
            let gd = o.stdout.trim();
            let gd_path = if Path::new(gd).is_absolute() { Path::new(gd).to_path_buf() } else { Path::new(path).join(gd) };
            s.last_fetch_ms = std::fs::metadata(gd_path.join("FETCH_HEAD"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
        }
    }
    s
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub path: String,
    /// synced | upToDate | skippedDirty | diverged | noUpstream | notRepo | error
    pub outcome: String,
    pub message: String,
}

fn res(path: &str, outcome: &str, message: impl Into<String>) -> SyncResult {
    SyncResult { path: path.to_string(), outcome: outcome.to_string(), message: message.into() }
}

pub fn fetch(path: &str) -> Result<(), String> {
    let o = git(path, &["fetch", "--prune"])?;
    if o.ok { Ok(()) } else { Err(o.error_text()) }
}

/// Fast-forward only. Never touches a dirty tree and never creates merge commits.
pub fn pull_safe(path: &str) -> SyncResult {
    let st = status(path);
    if !st.is_repo {
        return res(path, "notRepo", st.error.unwrap_or_else(|| "Not a git repository".into()));
    }
    if st.upstream.is_none() {
        return res(path, "noUpstream", format!("`{}` has no upstream branch", st.branch));
    }
    if let Err(e) = fetch(path) {
        return res(path, "error", format!("fetch failed: {e}"));
    }
    let st = status(path);
    if st.behind == 0 {
        return res(path, "upToDate", "Already up to date");
    }
    if st.ahead > 0 {
        return res(path, "diverged", format!("{} ahead / {} behind origin: needs a manual rebase or merge", st.ahead, st.behind));
    }
    let tracked_dirty = st.changes.len();
    if tracked_dirty > 0 {
        return res(path, "skippedDirty", format!("{tracked_dirty} uncommitted change(s): stash first, then pull"));
    }
    match git(path, &["pull", "--ff-only"]) {
        Ok(o) if o.ok => res(path, "synced", format!("Fast-forwarded {} commit(s)", st.behind)),
        Ok(o) => res(path, "error", o.error_text()),
        Err(e) => res(path, "error", e),
    }
}

/// Errors that mean "the index could not be locked/replaced right now". On Windows this is usually transient
/// (antivirus, OneDrive or an IDE briefly holding `.git/index`), so it is worth another attempt.
fn index_busy(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("could not write index") || t.contains("index.lock") || t.contains("unable to write new index") || t.contains("unable to create")
}

fn git_dir(path: &str) -> Option<std::path::PathBuf> {
    let o = git(path, &["rev-parse", "--git-dir"]).ok().filter(|o| o.ok)?;
    let gd = o.stdout.trim().to_string();
    Some(if Path::new(&gd).is_absolute() { Path::new(&gd).to_path_buf() } else { Path::new(path).join(gd) })
}

/// Turn a raw "could not write index" into something the user can act on.
fn explain(path: &str, text: String) -> String {
    if !index_busy(&text) {
        return text;
    }
    let lock = git_dir(path).map(|g| g.join("index.lock")).filter(|l| l.exists());
    match lock {
        Some(l) => format!(
            "{text}\n\nGit's index is locked: {} exists. Another git process (or an IDE/git client) is using this repo. If nothing is running, delete that file and try again.",
            l.display()
        ),
        None => format!("{text}\n\nGit could not update its index even after retrying. Something else is holding the repo's .git folder (antivirus, OneDrive sync, an IDE or another git tool). Wait a moment, close other git tools on this folder and try again."),
    }
}

/// Run a git command that writes the index, retrying when the index is only briefly busy.
fn git_write(path: &str, args: &[&str]) -> Result<Out, String> {
    let mut attempt = 0;
    loop {
        let o = git(path, args)?;
        if o.ok || attempt >= 3 || !index_busy(&o.error_text()) {
            return Ok(o);
        }
        attempt += 1;
        std::thread::sleep(std::time::Duration::from_millis(350 * attempt));
    }
}

fn stash_count(path: &str) -> usize {
    git(path, &["stash", "list"]).map(|o| o.stdout.lines().count()).unwrap_or(0)
}

/// Stash local changes under a name. `message` may be empty (git then names it after the branch and last commit).
pub fn stash(path: &str, message: &str, include_untracked: bool) -> Result<String, String> {
    let message = message.trim();
    let mut args = vec!["stash", "push"];
    if include_untracked {
        args.push("--include-untracked");
    }
    if !message.is_empty() {
        args.push("-m");
        args.push(message);
    }
    let before = stash_count(path);
    let mut attempt = 0;
    loop {
        let o = git(path, &args)?;
        if o.ok {
            let out = o.stdout.trim().to_string();
            return Ok(if out.is_empty() { "Stashed".into() } else { out });
        }
        let text = o.error_text();
        // The stash entry is created before the tree is reset: if that happened, retrying would stash twice.
        if stash_count(path) > before {
            return Err(format!("{text}\n\nThe changes were saved as stash@{{0}}, but git could not clean the working tree afterwards. Check the repo state before stashing again."));
        }
        if attempt >= 3 || !index_busy(&text) {
            return Err(explain(path, text));
        }
        attempt += 1;
        std::thread::sleep(std::time::Duration::from_millis(350 * attempt));
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StashEntry {
    pub index: u32,
    pub message: String,
    pub age: String,
}

pub fn stash_list(path: &str) -> Result<Vec<StashEntry>, String> {
    let o = git(path, &["stash", "list", "--format=%gd%x1f%gs%x1f%cr"])?;
    if !o.ok {
        return Err(o.error_text());
    }
    Ok(o.stdout
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\x1f');
            let (r, m, a) = (it.next()?, it.next()?, it.next()?);
            let index = r.trim_start_matches("stash@{").trim_end_matches('}').parse().ok()?;
            Some(StashEntry { index, message: m.to_string(), age: a.to_string() })
        })
        .collect())
}

/// Apply a stash and drop it (the latest one unless `index` is given). A conflicting pop keeps the stash.
pub fn stash_pop(path: &str, index: Option<u32>) -> Result<String, String> {
    let r = format!("stash@{{{}}}", index.unwrap_or(0));
    let o = git_write(path, &["stash", "pop", &r])?;
    if o.ok { Ok(format!("Applied {r}")) } else { Err(explain(path, o.error_text())) }
}

pub fn stash_drop(path: &str, index: u32) -> Result<String, String> {
    let r = format!("stash@{{{index}}}");
    let o = git_write(path, &["stash", "drop", &r])?;
    if o.ok { Ok(format!("Dropped {r}")) } else { Err(explain(path, o.error_text())) }
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub name: String,
    pub upstream: Option<String>,
    pub last_commit: String,
    pub current: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Branches {
    pub current: String,
    /// Local branches, current first.
    pub local: Vec<BranchInfo>,
    /// Remote branches that have no local branch yet (`origin/feature-x`).
    pub remote: Vec<BranchInfo>,
}

pub fn branches(path: &str) -> Result<Branches, String> {
    let o = git(
        path,
        &["for-each-ref", "--sort=-committerdate", "--format=%(refname)%09%(refname:short)%09%(upstream:short)%09%(objectname:short) %(subject)", "refs/heads", "refs/remotes"],
    )?;
    if !o.ok {
        return Err(o.error_text());
    }
    let cur = git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|o| o.ok).map(|o| o.stdout.trim().to_string()).unwrap_or_default();
    let mut b = Branches { current: cur.clone(), ..Default::default() };
    let mut remotes = vec![];
    for l in o.stdout.lines() {
        let mut it = l.splitn(4, '\t');
        let (full, short, up, last) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
        let info = BranchInfo { name: short.to_string(), upstream: (!up.is_empty()).then(|| up.to_string()), last_commit: last.to_string(), current: short == cur };
        if full.starts_with("refs/heads/") {
            b.local.push(info);
        } else if full.starts_with("refs/remotes/") && !short.ends_with("/HEAD") && short.contains('/') {
            remotes.push(info);
        }
    }
    b.local.sort_by_key(|x| !x.current);
    for r in remotes {
        let local_name = r.name.split_once('/').map(|x| x.1).unwrap_or("");
        if !b.local.iter().any(|l| l.name == local_name) {
            b.remote.push(r);
        }
    }
    Ok(b)
}

/// Switch to a branch. `remote` branches (`origin/x`) get a tracking local branch; `create` makes a new branch from HEAD.
/// With `stash_first`, local changes (incl. untracked) are stashed under a name before switching.
pub fn checkout(path: &str, branch: &str, create: bool, stash_first: bool) -> Result<String, String> {
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') || branch.contains(char::is_whitespace) {
        return Err(format!("`{branch}` is not a valid branch name"));
    }
    if create {
        let ok = git(path, &["check-ref-format", "--branch", branch])?;
        if !ok.ok {
            return Err(format!("`{branch}` is not a valid branch name"));
        }
    }
    let mut notes = vec![];
    if stash_first {
        let st = status(path);
        if !st.changes.is_empty() || !st.untracked.is_empty() {
            stash(path, &format!("auto-stash before switching to {branch}"), true)?;
            notes.push("local changes stashed".to_string());
        }
    }
    let is_remote = !create && git(path, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{branch}")]).map(|o| o.ok).unwrap_or(false)
        && !git(path, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).map(|o| o.ok).unwrap_or(false);
    let args: Vec<&str> = if create {
        vec!["checkout", "-b", branch]
    } else if is_remote {
        vec!["checkout", "--track", branch]
    } else {
        vec!["checkout", branch]
    };
    let o = git_write(path, &args)?;
    if !o.ok {
        return Err(explain(path, o.error_text()));
    }
    let now = status(path).branch;
    let extra = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join(", ")) };
    Ok(format!("Now on {now}{extra}"))
}

pub fn diff_file(path: &str, file: &str) -> Result<String, String> {
    let o = git(path, &["diff", "HEAD", "--", file])?;
    if o.ok { Ok(o.stdout) } else { Err(o.error_text()) }
}
