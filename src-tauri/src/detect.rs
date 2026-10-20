use serde::Serialize;
use std::path::Path;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub path: String,
    pub name: String,
    pub stack: String,
    pub command: String,
    pub port: Option<u16>,
    pub is_git: bool,
    pub notes: Vec<String>,
}

fn find_port(text: &str) -> Option<u16> {
    for marker in ["--port ", "--port=", "-p ", "PORT=", "PORT "] {
        if let Some(i) = text.find(marker) {
            let rest = &text[i + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = digits.parse::<u16>() {
                if p >= 80 {
                    return Some(p);
                }
            }
        }
    }
    None
}

pub fn detect(path: &str) -> Result<Detected, String> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err("Not a folder".into());
    }
    let mut d = Detected {
        path: path.to_string(),
        name: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "service".into()),
        is_git: p.join(".git").exists(),
        ..Default::default()
    };

    let pkg = p.join("package.json");
    if pkg.is_file() {
        d.stack = "Node.js".into();
        let runner = if p.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if p.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm"
        };
        let json: serde_json::Value = std::fs::read_to_string(&pkg)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if let Some(n) = json.get("name").and_then(|v| v.as_str()) {
            let short = n.rsplit('/').next().unwrap_or(n);
            if !short.is_empty() {
                d.name = short.to_string();
            }
        }
        let scripts = json.get("scripts");
        let pick = ["dev", "start", "serve"].into_iter().find(|k| scripts.and_then(|s| s.get(*k)).is_some());
        match pick {
            Some(k) => {
                d.command = if k == "start" && runner == "npm" { "npm start".into() } else { format!("{runner} run {k}") };
                let body = scripts.and_then(|s| s.get(k)).and_then(|v| v.as_str()).unwrap_or("");
                d.port = find_port(body);
                if d.port.is_none() {
                    // Don't guess a framework default: with no port set, the real one is detected once the service runs.
                    d.notes.push("No port in the script: leave the port empty and it is detected when the service starts, or set one to pin it.".into());
                }
            }
            None => d.notes.push("No dev/start script found in package.json.".into()),
        }
    } else if p.join("Cargo.toml").is_file() {
        d.stack = "Rust".into();
        d.command = "cargo run".into();
    } else if p.join("go.mod").is_file() {
        d.stack = "Go".into();
        d.command = "go run .".into();
    } else if p.join("manage.py").is_file() {
        d.stack = "Python (Django)".into();
        d.command = "python manage.py runserver".into();
        d.port = Some(8000);
    } else if p.join("pyproject.toml").is_file() || p.join("requirements.txt").is_file() || p.join("main.py").is_file() {
        d.stack = "Python".into();
        if p.join("main.py").is_file() {
            d.command = "python main.py".into();
        }
    } else if p.join("pom.xml").is_file() {
        d.stack = "Java (Maven)".into();
        d.command = "mvn spring-boot:run".into();
        d.port = Some(8080);
    } else if p.join("build.gradle").is_file() || p.join("build.gradle.kts").is_file() {
        d.stack = "Java (Gradle)".into();
        d.command = "gradlew bootRun".into();
        d.port = Some(8080);
    } else if p.join("docker-compose.yml").is_file() || p.join("compose.yaml").is_file() || p.join("docker-compose.yaml").is_file() {
        d.stack = "Docker Compose".into();
        d.command = "docker compose up".into();
    } else {
        d.stack = "Unknown".into();
        d.notes.push("Could not detect the project type: enter the start command manually.".into());
    }
    Ok(d)
}
