mod chaos;
mod ci;
mod detect;
mod docker;
mod git;
mod model;
mod portargs;
mod ports;
mod procs;
mod shell;
mod storage;
mod store;
mod up;
mod util;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use tauri::{Manager, RunEvent, State};

use model::{Settings, StoreData, Workspace};
use procs::{LogPage, Runtime, ServiceRuntime, SystemStats};
use store::Store;

pub struct AppState {
    store: Arc<Store>,
    rt: Arc<Runtime>,
    ci: Arc<ci::engine::Ci>,
}

type CmdResult<T> = Result<T, String>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn ws_of(state: &AppState, id: &str) -> CmdResult<Workspace> {
    state.store.workspace(id).ok_or_else(|| "Workspace not found".to_string())
}

// ---------- store ----------

#[tauri::command]
fn get_store(state: State<'_, AppState>) -> StoreData {
    state.store.snapshot()
}

#[tauri::command]
fn save_workspace(state: State<'_, AppState>, workspace: Workspace) -> CmdResult<StoreData> {
    state.store.save_workspace(workspace)
}

#[tauri::command]
fn delete_workspace(state: State<'_, AppState>, id: String) -> CmdResult<StoreData> {
    state.rt.stop_all(&id);
    state.rt.clear_logs(&id);
    state.store.delete_workspace(&id)
}

#[tauri::command]
fn set_active_workspace(state: State<'_, AppState>, id: String) -> CmdResult<StoreData> {
    state.store.set_active(&id)
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<StoreData> {
    state.store.save_settings(settings)
}

#[tauri::command]
async fn detect_project(path: String) -> CmdResult<detect::Detected> {
    blocking(move || detect::detect(&path)).await?
}

/// What will really be executed for a service, given its port (so the editor can show it).
#[tauri::command]
async fn preview_command(repo_path: String, command: String, port: Option<u16>) -> CmdResult<portargs::Applied> {
    blocking(move || portargs::apply(&repo_path, &command, port)).await
}

// ---------- git ----------

/// What Stackyard keeps on disk and in Docker (sizes can take a few seconds).
#[tauri::command]
async fn storage_report(state: State<'_, AppState>) -> CmdResult<storage::StorageReport> {
    let ci = state.ci.clone();
    blocking(move || storage::report(&ci)).await
}

#[tauri::command]
async fn storage_clean(state: State<'_, AppState>, items: Vec<String>) -> CmdResult<storage::CleanResult> {
    let ci = state.ci.clone();
    blocking(move || storage::clean(&ci, &items)).await?
}

/// Terminals a service's start command can run in, and which of them are installed.
#[tauri::command]
async fn shell_options() -> CmdResult<Vec<shell::ShellInfo>> {
    blocking(shell::available).await
}

#[tauri::command]
async fn git_status(paths: Vec<String>) -> CmdResult<Vec<git::GitStatus>> {
    blocking(move || {
        let handles: Vec<_> = paths.into_iter().map(|p| std::thread::spawn(move || git::status(&p))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
    .await
}

#[tauri::command]
async fn git_fetch(paths: Vec<String>) -> CmdResult<Vec<git::SyncResult>> {
    blocking(move || {
        let handles: Vec<_> = paths
            .into_iter()
            .map(|p| {
                std::thread::spawn(move || match git::fetch(&p) {
                    Ok(()) => git::SyncResult { path: p, outcome: "upToDate".into(), message: "Fetched".into() },
                    Err(e) => git::SyncResult { path: p, outcome: "error".into(), message: e },
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
    .await
}

#[tauri::command]
async fn git_pull_safe(paths: Vec<String>) -> CmdResult<Vec<git::SyncResult>> {
    blocking(move || {
        let handles: Vec<_> = paths.into_iter().map(|p| std::thread::spawn(move || git::pull_safe(&p))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
    .await
}

#[tauri::command]
async fn git_stash(path: String, message: Option<String>, include_untracked: Option<bool>) -> CmdResult<String> {
    blocking(move || git::stash(&path, message.as_deref().unwrap_or(""), include_untracked.unwrap_or(true))).await?
}

#[tauri::command]
async fn git_stash_list(path: String) -> CmdResult<Vec<git::StashEntry>> {
    blocking(move || git::stash_list(&path)).await?
}

#[tauri::command]
async fn git_stash_pop(path: String, index: Option<u32>) -> CmdResult<String> {
    blocking(move || git::stash_pop(&path, index)).await?
}

#[tauri::command]
async fn git_stash_drop(path: String, index: u32) -> CmdResult<String> {
    blocking(move || git::stash_drop(&path, index)).await?
}

#[tauri::command]
async fn git_branches(path: String) -> CmdResult<git::Branches> {
    blocking(move || git::branches(&path)).await?
}

#[tauri::command]
async fn git_checkout(path: String, branch: String, create: Option<bool>, stash_first: Option<bool>) -> CmdResult<String> {
    blocking(move || git::checkout(&path, &branch, create.unwrap_or(false), stash_first.unwrap_or(false))).await?
}

#[tauri::command]
async fn git_diff_file(path: String, file: String) -> CmdResult<String> {
    blocking(move || git::diff_file(&path, &file)).await?
}

// ---------- runtime ----------

#[tauri::command]
async fn runtime_status(state: State<'_, AppState>, workspace_id: String) -> CmdResult<Vec<ServiceRuntime>> {
    let ws = ws_of(&state, &workspace_id)?;
    let rt = state.rt.clone();
    blocking(move || rt.status(&ws)).await
}

#[tauri::command]
async fn service_start(state: State<'_, AppState>, workspace_id: String, name: String) -> CmdResult<()> {
    let ws = ws_of(&state, &workspace_id)?;
    let svc = ws.services.iter().find(|s| s.name == name).cloned().ok_or("Service not found")?;
    let rt = state.rt.clone();
    blocking(move || rt.start_service(&ws, &svc).map_err(|e| e.message)).await?
}

#[tauri::command]
async fn service_stop(state: State<'_, AppState>, workspace_id: String, name: String) -> CmdResult<()> {
    let rt = state.rt.clone();
    blocking(move || rt.stop_service(&workspace_id, &name)).await?
}

#[tauri::command]
async fn service_restart(state: State<'_, AppState>, workspace_id: String, name: String) -> CmdResult<()> {
    let ws = ws_of(&state, &workspace_id)?;
    let svc = ws.services.iter().find(|s| s.name == name).cloned().ok_or("Service not found")?;
    let rt = state.rt.clone();
    blocking(move || {
        rt.stop_service(&ws.id, &svc.name)?;
        for _ in 0..80 {
            if !rt.is_alive(&ws.id, &svc.name) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        rt.start_service(&ws, &svc).map_err(|e| e.message)
    })
    .await?
}

// ---------- fault injection ----------

#[tauri::command]
fn chaos_status(state: State<'_, AppState>, workspace_id: String) -> CmdResult<Vec<procs::ChaosState>> {
    let ws = ws_of(&state, &workspace_id)?;
    Ok(state.rt.chaos_states(&ws))
}

#[tauri::command]
fn chaos_set(state: State<'_, AppState>, workspace_id: String, service: String, faults: chaos::Faults) -> CmdResult<()> {
    let ws = ws_of(&state, &workspace_id)?;
    if !ws.services.iter().any(|s| s.name == service) {
        return Err("Service not found".into());
    }
    state.rt.chaos_set(&workspace_id, &service, faults);
    Ok(())
}

#[tauri::command]
fn chaos_reset(state: State<'_, AppState>, workspace_id: String, service: Option<String>) {
    state.rt.chaos_reset(&workspace_id, service.as_deref());
}

/// Turn the fault proxy on/off for a service. A running service is restarted so it moves behind (or out
/// from behind) the proxy.
#[tauri::command]
async fn chaos_enable(state: State<'_, AppState>, workspace_id: String, service: String, enabled: bool) -> CmdResult<StoreData> {
    let mut ws = ws_of(&state, &workspace_id)?;
    let svc = ws.services.iter_mut().find(|s| s.name == service).ok_or("Service not found")?;
    if enabled && svc.port.is_none() {
        return Err("Give this service a port first: the fault proxy needs a port to sit on.".into());
    }
    svc.chaos = enabled;
    let data = state.store.save_workspace(ws.clone())?;
    if !enabled {
        state.rt.chaos_reset(&workspace_id, Some(&service));
    }
    let svc = ws.services.iter().find(|s| s.name == service).cloned().ok_or("Service not found")?;
    let rt = state.rt.clone();
    blocking(move || -> Result<(), String> {
        if rt.is_alive(&ws.id, &svc.name) {
            rt.stop_service(&ws.id, &svc.name)?;
            for _ in 0..80 {
                if !rt.is_alive(&ws.id, &svc.name) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            rt.start_service(&ws, &svc).map_err(|e| e.message)?;
        }
        Ok(())
    })
    .await??;
    Ok(data)
}

#[tauri::command]
async fn chaos_probe(state: State<'_, AppState>, workspace_id: String, service: String, path: Option<String>) -> CmdResult<chaos::ProbeResult> {
    let ws = ws_of(&state, &workspace_id)?;
    let svc = ws.services.iter().find(|s| s.name == service).cloned().ok_or("Service not found")?;
    let rt = state.rt.clone();
    blocking(move || rt.chaos_probe(&svc, path.as_deref().unwrap_or("/"))).await?
}

#[tauri::command]
async fn stop_all(state: State<'_, AppState>, workspace_id: String) -> CmdResult<()> {
    let rt = state.rt.clone();
    blocking(move || rt.stop_all(&workspace_id)).await
}

#[tauri::command]
fn get_logs(state: State<'_, AppState>, workspace_id: String, after: u64, limit: usize) -> LogPage {
    state.rt.logs_after(&workspace_id, after, limit.min(5000))
}

#[tauri::command]
fn clear_logs(state: State<'_, AppState>, workspace_id: String) {
    state.rt.clear_logs(&workspace_id)
}

// ---------- up pipeline ----------

#[tauri::command]
fn up_start(state: State<'_, AppState>, workspace_id: String) -> CmdResult<()> {
    let ws = ws_of(&state, &workspace_id)?;
    up::start(state.rt.clone(), ws, state.store.settings())
}

#[tauri::command]
fn up_cancel(state: State<'_, AppState>) {
    state.rt.up_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command]
fn up_status(state: State<'_, AppState>) -> Option<up::UpRun> {
    state.rt.up.lock().unwrap().clone()
}

// ---------- CI simulator ----------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoPipelines {
    repo_path: String,
    pipelines: Vec<ci::ir::PipelineFile>,
}

/// Pipeline definitions found in the repos of a workspace.
#[tauri::command]
async fn ci_pipelines(state: State<'_, AppState>, workspace_id: String) -> CmdResult<Vec<RepoPipelines>> {
    let ws = ws_of(&state, &workspace_id)?;
    let ci = state.ci.clone();
    blocking(move || {
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for s in &ws.services {
            let p = s.repo_path.trim().to_string();
            if p.is_empty() || !seen.insert(p.clone()) {
                continue;
            }
            out.push(RepoPipelines { pipelines: ci.pipelines(&p), repo_path: p });
        }
        out
    })
    .await
}

#[tauri::command]
async fn ci_pipeline(state: State<'_, AppState>, repo_path: String, provider: String, file: String) -> CmdResult<ci::ir::Pipeline> {
    let ci = state.ci.clone();
    blocking(move || ci.load(&repo_path, &provider, &file)).await?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CiRefs {
    branches: Vec<String>,
    tags: Vec<String>,
    current: String,
}

#[tauri::command]
async fn ci_refs(repo_path: String) -> CmdResult<CiRefs> {
    blocking(move || {
        let (branches, tags, current) = ci::engine::git_refs(&repo_path);
        CiRefs { branches, tags, current }
    })
    .await
}

#[tauri::command]
async fn ci_start(state: State<'_, AppState>, workspace_id: String, options: ci::engine::RunOptions) -> CmdResult<String> {
    let ws = ws_of(&state, &workspace_id)?;
    let ci = state.ci.clone();
    let mut options = options;
    if options.ubuntu_image.trim().is_empty() {
        // The runner image chosen on the Plugins page.
        options.ubuntu_image = state.store.settings().ci_default_image;
    }
    blocking(move || ci.start(options, ws.ci_vars.clone())).await?
}

#[tauri::command]
fn ci_runs(state: State<'_, AppState>) -> Vec<ci::engine::RunView> {
    state.ci.list()
}

#[tauri::command]
fn ci_run(state: State<'_, AppState>, run_id: String) -> Option<ci::engine::RunView> {
    state.ci.get(&run_id)
}

#[tauri::command]
fn ci_log(state: State<'_, AppState>, run_id: String, job: String, after: usize) -> Option<ci::engine::LogPage> {
    state.ci.log(&run_id, &job, after)
}

#[tauri::command]
async fn ci_cancel(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    let ci = state.ci.clone();
    blocking(move || ci.cancel(&run_id)).await
}

#[tauri::command]
async fn ci_delete(state: State<'_, AppState>, run_id: String) -> CmdResult<()> {
    let ci = state.ci.clone();
    blocking(move || ci.delete(&run_id)).await
}

#[tauri::command]
fn ci_play(state: State<'_, AppState>, run_id: String, job: String, steps: Option<Vec<usize>>) -> CmdResult<()> {
    state.ci.play(&run_id, &job, steps.unwrap_or_default())
}

#[tauri::command]
async fn ci_docker_ready() -> CmdResult<bool> {
    blocking(ci::exec::docker_ready).await
}

// ---------- ports ----------

#[tauri::command]
async fn list_listeners() -> CmdResult<Vec<ports::Listener>> {
    blocking(ports::listeners).await
}

#[tauri::command]
async fn kill_pid(pid: u32) -> CmdResult<()> {
    blocking(move || ports::kill_pid(pid)).await?
}

// ---------- docker ----------

#[tauri::command]
async fn docker_overview() -> CmdResult<docker::DockerOverview> {
    blocking(docker::overview).await
}

#[tauri::command]
async fn docker_action(kind: String, target: String) -> CmdResult<String> {
    blocking(move || docker::action(&kind, &target)).await?
}

// ---------- system ----------

#[tauri::command]
fn system_stats(state: State<'_, AppState>) -> SystemStats {
    state.rt.system_stats()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            // The app used to be called DevWorkspace (identifier com.devworkspace.app): bring its saved
            // workspaces and settings along the first time the renamed app starts.
            if !dir.join("store.json").exists() {
                if let Some(old) = dir.parent().map(|p| p.join("com.devworkspace.app").join("store.json")).filter(|p| p.exists()) {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::copy(old, dir.join("store.json"));
                }
            }
            let store = Arc::new(Store::load(dir.join("store.json")));
            // A previous session that was force-killed can leave managed containers behind
            // (they would keep holding their ports). We lose track of them across restarts, so remove them.
            if store.settings().stop_on_exit {
                std::thread::spawn(|| {
                    docker::cleanup_managed("dw-");
                });
            }
            // Pipeline containers/networks left over from a crashed session.
            std::thread::spawn(ci::exec::cleanup_leftovers);
            // Keep the pipeline caches from growing forever (old entries, then oldest-first above the size cap).
            let ci_dir = dir.join("ci");
            std::thread::spawn(move || {
                storage::prune_caches(&ci_dir);
            });
            app.manage(AppState { store, rt: Runtime::new(), ci: Arc::new(ci::engine::Ci::new(dir.join("ci"))) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_store,
            save_workspace,
            delete_workspace,
            set_active_workspace,
            save_settings,
            detect_project,
            preview_command,
            git_status,
            git_fetch,
            git_pull_safe,
            shell_options,
            storage_report,
            storage_clean,
            git_stash,
            git_stash_pop,
            git_stash_list,
            git_stash_drop,
            git_branches,
            git_checkout,
            git_diff_file,
            runtime_status,
            service_start,
            service_stop,
            service_restart,
            chaos_status,
            chaos_set,
            chaos_reset,
            chaos_enable,
            chaos_probe,
            stop_all,
            get_logs,
            clear_logs,
            up_start,
            up_cancel,
            up_status,
            list_listeners,
            kill_pid,
            docker_overview,
            docker_action,
            system_stats,
            ci_pipelines,
            ci_pipeline,
            ci_refs,
            ci_start,
            ci_runs,
            ci_run,
            ci_log,
            ci_cancel,
            ci_delete,
            ci_play,
            ci_docker_ready,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                state.ci.cancel_all();
                if state.store.settings().stop_on_exit {
                    state.rt.stop_everything();
                }
            }
        }
    });
}

