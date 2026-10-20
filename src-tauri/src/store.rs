use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::model::{Settings, StoreData, Workspace};
use crate::util::now_ms;

pub struct Store {
    path: PathBuf,
    data: Mutex<StoreData>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let data = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<StoreData>(&s).ok())
            .unwrap_or_default();
        Store { path, data: Mutex::new(data) }
    }

    pub fn snapshot(&self) -> StoreData {
        self.data.lock().unwrap().clone()
    }

    pub fn workspace(&self, id: &str) -> Option<Workspace> {
        self.data.lock().unwrap().workspaces.iter().find(|w| w.id == id).cloned()
    }

    fn persist(&self, d: &StoreData) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(d).map_err(|e| e.to_string())?;
        // Write atomically so a crash can't leave a half-written file.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn save_workspace(&self, mut ws: Workspace) -> Result<StoreData, String> {
        if ws.name.trim().is_empty() {
            return Err("Workspace name is required".into());
        }
        let mut seen = std::collections::HashSet::new();
        for s in &ws.services {
            if s.name.trim().is_empty() {
                return Err("Every service needs a name".into());
            }
            if !seen.insert(s.name.clone()) {
                return Err(format!("Duplicate service name `{}`", s.name));
            }
        }
        let mut d = self.data.lock().unwrap();
        if ws.id.is_empty() {
            ws.id = format!("ws-{}", now_ms());
        }
        if let Some(existing) = d.workspaces.iter_mut().find(|w| w.id == ws.id) {
            *existing = ws.clone();
        } else {
            d.workspaces.push(ws.clone());
        }
        if d.active.is_none() {
            d.active = Some(ws.id.clone());
        }
        self.persist(&d)?;
        Ok(d.clone())
    }

    pub fn delete_workspace(&self, id: &str) -> Result<StoreData, String> {
        let mut d = self.data.lock().unwrap();
        d.workspaces.retain(|w| w.id != id);
        if d.active.as_deref() == Some(id) {
            d.active = d.workspaces.first().map(|w| w.id.clone());
        }
        self.persist(&d)?;
        Ok(d.clone())
    }

    pub fn set_active(&self, id: &str) -> Result<StoreData, String> {
        let mut d = self.data.lock().unwrap();
        if !d.workspaces.iter().any(|w| w.id == id) {
            return Err("Unknown workspace".into());
        }
        d.active = Some(id.to_string());
        self.persist(&d)?;
        Ok(d.clone())
    }

    pub fn save_settings(&self, s: Settings) -> Result<StoreData, String> {
        let mut d = self.data.lock().unwrap();
        d.settings = s;
        self.persist(&d)?;
        Ok(d.clone())
    }

    pub fn settings(&self) -> Settings {
        self.data.lock().unwrap().settings.clone()
    }
}
