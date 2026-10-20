use serde::Serialize;
use serde_json::Value;

use crate::util::run;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DockerOverview {
    pub available: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub containers: Vec<Value>,
    pub volumes: Vec<Value>,
    pub images: Vec<Value>,
}

fn json_lines(args: &[&str]) -> Vec<Value> {
    run("docker", args, None)
        .ok()
        .filter(|o| o.ok)
        .map(|o| {
            o.stdout
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn overview() -> DockerOverview {
    let mut d = DockerOverview::default();
    match run("docker", &["version", "--format", "{{.Server.Version}}"], None) {
        Ok(o) if o.ok && !o.stdout.trim().is_empty() => {
            d.available = true;
            d.version = Some(o.stdout.trim().to_string());
        }
        Ok(o) => {
            let t = o.error_text();
            d.error = Some(if t.to_lowercase().contains("pipe") || t.to_lowercase().contains("daemon") || t.to_lowercase().contains("connect") {
                "Docker is installed but the daemon is not running. Start Docker Desktop.".into()
            } else if t.is_empty() {
                "Docker did not respond".into()
            } else {
                t
            });
            return d;
        }
        Err(_) => {
            d.error = Some("Docker CLI not found on PATH".into());
            return d;
        }
    }
    d.containers = json_lines(&["ps", "-a", "--format", "{{json .}}"]);
    d.volumes = json_lines(&["volume", "ls", "--format", "{{json .}}"]);
    d.images = json_lines(&["images", "--format", "{{json .}}"]);
    d
}

/// Remove containers whose name starts with `prefix` (leftovers of a crashed/force-killed session).
/// Returns how many were removed.
pub fn cleanup_managed(prefix: &str) -> usize {
    let filter = format!("name=^{prefix}");
    let ids: Vec<String> = match run("docker", &["ps", "-aq", "--filter", &filter], None) {
        Ok(o) if o.ok => o.stdout.split_whitespace().map(String::from).collect(),
        _ => return 0,
    };
    if ids.is_empty() {
        return 0;
    }
    let mut args: Vec<&str> = vec!["rm", "-f"];
    args.extend(ids.iter().map(|s| s.as_str()));
    let _ = run("docker", &args, None);
    ids.len()
}

#[cfg(test)]
pub fn image_present(image: &str) -> bool {
    run("docker", &["image", "inspect", image], None).map(|o| o.ok).unwrap_or(false)
}

pub fn is_running() -> bool {
    run("docker", &["version", "--format", "{{.Server.Version}}"], None)
        .map(|o| o.ok && !o.stdout.trim().is_empty())
        .unwrap_or(false)
}

/// Whitelisted docker actions. `target` is an id/name/image.
pub fn action(kind: &str, target: &str) -> Result<String, String> {
    if target.trim().is_empty() || target.starts_with('-') {
        return Err("Invalid target".into());
    }
    let args: Vec<&str> = match kind {
        "start" => vec!["start", target],
        "stop" => vec!["stop", "-t", "5", target],
        "restart" => vec!["restart", "-t", "5", target],
        "rm" => vec!["rm", "-f", target],
        "volume_rm" => vec!["volume", "rm", target],
        "image_rm" => vec!["rmi", target],
        "pull" => vec!["pull", target],
        _ => return Err(format!("Unknown docker action `{kind}`")),
    };
    let o = run("docker", &args, None)?;
    if o.ok { Ok(o.stdout.trim().to_string()) } else { Err(o.error_text()) }
}
