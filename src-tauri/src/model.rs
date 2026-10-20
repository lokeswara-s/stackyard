use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub secret: bool,
    /// `None` = applies to every profile.
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    /// "app" (runs a local command) or "infra" (runs a Docker image).
    pub kind: String,
    #[serde(default)]
    pub stack: String,
    /// Working directory (usually the repo root) for app services.
    #[serde(default)]
    pub repo_path: String,
    #[serde(default)]
    pub command: String,
    /// Terminal the start command runs in: "" (default: Command Prompt on Windows, sh elsewhere) | cmd | powershell | bash.
    #[serde(default)]
    pub shell: String,
    /// Skip the terminal's own profile (`$PROFILE`, `.bash_profile`, cmd AutoRun). Off by default: like a regular terminal.
    #[serde(default)]
    pub no_profile: bool,
    /// A script sourced in the same terminal session right before the start command (functions, aliases, PATH).
    #[serde(default)]
    pub shell_init: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub container_port: Option<u16>,
    /// Path inside the container that should persist in a named volume.
    #[serde(default)]
    pub data_path: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Route this service's port through the fault-injection proxy (the service itself moves to a hidden port).
    #[serde(default)]
    pub chaos: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// local | staging-like | prod-sim
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default = "default_true")]
    pub sync_before_up: bool,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Variables and secrets for the CI simulator (GitLab CI/CD variables, GitHub secrets, Jenkins credentials).
    #[serde(default)]
    pub ci_vars: Vec<EnvVar>,
    /// Tools the workspace needs on this machine (`Java 8` = `java -version`), checked on demand.
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Prerequisite {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub command: String,
    /// Optional text the output must contain (case-insensitive), e.g. `v24` or `1.8.0`, to check the version.
    #[serde(default)]
    pub expected: String,
}

fn default_profile() -> String {
    "local".into()
}
fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub health_timeout_secs: u64,
    pub stop_on_exit: bool,
    /// Runner images the user added for the CI simulator (pulled once from the Plugins page, reused by every run).
    #[serde(default)]
    pub ci_images: Vec<String>,
    /// Image for CI jobs that do not name one (empty = built-in default).
    #[serde(default)]
    pub ci_default_image: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { health_timeout_secs: 60, stop_on_exit: true, ci_images: vec![], ci_default_image: String::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoreData {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub settings: Settings,
}

/// Env vars that apply to `svc` under `profile` (global first, service-specific wins).
pub fn resolve_env(ws: &Workspace, svc: &Service, profile: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |k: &str, v: &str| {
        if let Some(e) = out.iter_mut().find(|(ek, _)| ek == k) {
            e.1 = v.to_string();
        } else {
            out.push((k.to_string(), v.to_string()));
        }
    };
    // Precedence (lowest to highest): global/all-profiles, global/profile, service/all-profiles,
    // service/profile. Row order inside the editor never changes the outcome.
    for scope in [&ws.env, &svc.env] {
        for want_profile_specific in [false, true] {
            for v in scope.iter() {
                if v.key.trim().is_empty() {
                    continue;
                }
                let applies = match v.profile.as_deref() {
                    None | Some("") => !want_profile_specific,
                    Some(p) => want_profile_specific && p == profile,
                };
                if applies {
                    put(v.key.trim(), &v.value);
                }
            }
        }
    }
    if let Some(p) = svc.port {
        if !out.iter().any(|(k, _)| k == "PORT") {
            out.push(("PORT".into(), p.to_string()));
        }
    }
    out
}
