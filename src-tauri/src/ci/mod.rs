//! CI simulator: runs GitLab CI, GitHub Actions and Jenkins pipelines locally the way a server would
//! (clean checkout, containers, services, artifacts, cache, provider variables).

#![allow(dead_code)]

pub mod engine;
pub mod exec;
pub mod expr;
pub mod gh;
pub mod github;
pub mod gitlab;
pub mod groovy;
pub mod ir;
pub mod jenkins;
pub mod runner;

#[cfg(test)]
mod tests;

use std::path::Path;

use ir::{Pipeline, PipelineFile};

/// Pipeline definitions found in a repository.
pub fn discover(repo: &Path) -> Vec<PipelineFile> {
    let mut out = vec![];
    if repo.join(".gitlab-ci.yml").is_file() {
        out.push(PipelineFile { provider: "gitlab".into(), file: ".gitlab-ci.yml".into() });
    }
    if let Ok(rd) = std::fs::read_dir(repo.join(".github").join("workflows")) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
            .collect();
        names.sort();
        for n in names {
            out.push(PipelineFile { provider: "github".into(), file: format!(".github/workflows/{n}") });
        }
    }
    if let Ok(rd) = std::fs::read_dir(repo) {
        let mut names: Vec<String> = rd.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n == "Jenkinsfile" || n.starts_with("Jenkinsfile.")).collect();
        names.sort();
        for n in names {
            out.push(PipelineFile { provider: "jenkins".into(), file: n });
        }
    }
    out
}

pub fn load(repo: &Path, provider: &str, file: &str) -> Result<Pipeline, String> {
    if file.contains("..") {
        return Err("invalid file path".into());
    }
    match provider {
        "gitlab" => gitlab::parse(repo, file),
        "github" => github::parse(repo, file),
        "jenkins" => jenkins::parse(repo, file),
        other => Err(format!("unknown CI provider `{other}`")),
    }
}
