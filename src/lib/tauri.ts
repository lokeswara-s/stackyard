import { invoke } from "@tauri-apps/api/core";

import type {
  AppliedCommand,
  Branches,
  CleanResult,
  ShellInfo,
  StorageReport,
  StashEntry,
  ChaosState,
  CiLogPage,
  CiPipeline,
  CiRefs,
  CiRepoPipelines,
  CiRunOptions,
  CiRunView,
  Detected,
  Faults,
  ProbeResult,
  DockerOverview,
  GitStatus,
  Listener,
  LogLine,
  ServiceRuntime,
  Settings,
  StoreData,
  SyncResult,
  SystemStats,
  UpRun,
  Workspace,
} from "./types";

/** True when running inside the Tauri shell (false in a plain browser tab). */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error("Stackyard backend is only available inside the desktop app (run `npm run tauri dev`).");
  }
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));
  }
}

export const api = {
  getStore: () => call<StoreData>("get_store"),
  saveWorkspace: (workspace: Workspace) => call<StoreData>("save_workspace", { workspace }),
  deleteWorkspace: (id: string) => call<StoreData>("delete_workspace", { id }),
  setActiveWorkspace: (id: string) => call<StoreData>("set_active_workspace", { id }),
  saveSettings: (settings: Settings) => call<StoreData>("save_settings", { settings }),
  detectProject: (path: string) => call<Detected>("detect_project", { path }),
  previewCommand: (repoPath: string, command: string, port: number | null) =>
    call<AppliedCommand>("preview_command", { repoPath, command, port }),

  gitStatus: (paths: string[]) => call<GitStatus[]>("git_status", { paths }),
  gitFetch: (paths: string[]) => call<SyncResult[]>("git_fetch", { paths }),
  gitPullSafe: (paths: string[]) => call<SyncResult[]>("git_pull_safe", { paths }),
  shellOptions: () => call<ShellInfo[]>("shell_options"),
  storageReport: () => call<StorageReport>("storage_report"),
  storageClean: (items: string[]) => call<CleanResult>("storage_clean", { items }),
  gitStash: (path: string, message = "", includeUntracked = true) => call<string>("git_stash", { path, message, includeUntracked }),
  gitStashList: (path: string) => call<StashEntry[]>("git_stash_list", { path }),
  gitStashPop: (path: string, index?: number) => call<string>("git_stash_pop", { path, index: index ?? null }),
  gitStashDrop: (path: string, index: number) => call<string>("git_stash_drop", { path, index }),
  gitBranches: (path: string) => call<Branches>("git_branches", { path }),
  gitCheckout: (path: string, branch: string, create = false, stashFirst = false) => call<string>("git_checkout", { path, branch, create, stashFirst }),
  gitDiffFile: (path: string, file: string) => call<string>("git_diff_file", { path, file }),

  runtimeStatus: (workspaceId: string) => call<ServiceRuntime[]>("runtime_status", { workspaceId }),
  serviceStart: (workspaceId: string, name: string) => call<void>("service_start", { workspaceId, name }),
  serviceStop: (workspaceId: string, name: string) => call<void>("service_stop", { workspaceId, name }),
  serviceRestart: (workspaceId: string, name: string) => call<void>("service_restart", { workspaceId, name }),
  stopAll: (workspaceId: string) => call<void>("stop_all", { workspaceId }),

  chaosStatus: (workspaceId: string) => call<ChaosState[]>("chaos_status", { workspaceId }),
  chaosSet: (workspaceId: string, service: string, faults: Faults) => call<void>("chaos_set", { workspaceId, service, faults }),
  chaosReset: (workspaceId: string, service?: string) => call<void>("chaos_reset", { workspaceId, service: service ?? null }),
  chaosEnable: (workspaceId: string, service: string, enabled: boolean) => call<StoreData>("chaos_enable", { workspaceId, service, enabled }),
  chaosProbe: (workspaceId: string, service: string, path?: string) => call<ProbeResult>("chaos_probe", { workspaceId, service, path: path ?? null }),
  getLogs: (workspaceId: string, after: number, limit = 2000) =>
    call<{ lines: LogLine[]; lastSeq: number }>("get_logs", { workspaceId, after, limit }),
  clearLogs: (workspaceId: string) => call<void>("clear_logs", { workspaceId }),

  upStart: (workspaceId: string) => call<void>("up_start", { workspaceId }),
  upCancel: () => call<void>("up_cancel"),
  upStatus: () => call<UpRun | null>("up_status"),

  listListeners: () => call<Listener[]>("list_listeners"),
  killPid: (pid: number) => call<void>("kill_pid", { pid }),

  dockerOverview: () => call<DockerOverview>("docker_overview"),
  dockerAction: (kind: string, target: string) => call<string>("docker_action", { kind, target }),

  systemStats: () => call<SystemStats>("system_stats"),

  ciPipelines: (workspaceId: string) => call<CiRepoPipelines[]>("ci_pipelines", { workspaceId }),
  ciPipeline: (repoPath: string, provider: string, file: string) => call<CiPipeline>("ci_pipeline", { repoPath, provider, file }),
  ciRefs: (repoPath: string) => call<CiRefs>("ci_refs", { repoPath }),
  ciStart: (workspaceId: string, options: CiRunOptions) => call<string>("ci_start", { workspaceId, options }),
  ciRuns: () => call<CiRunView[]>("ci_runs"),
  ciRun: (runId: string) => call<CiRunView | null>("ci_run", { runId }),
  ciLog: (runId: string, job: string, after: number) => call<CiLogPage | null>("ci_log", { runId, job, after }),
  ciCancel: (runId: string) => call<void>("ci_cancel", { runId }),
  ciDelete: (runId: string) => call<void>("ci_delete", { runId }),
  ciPlay: (runId: string, job: string, steps?: number[]) => call<void>("ci_play", { runId, job, steps: steps ?? null }),
  ciDockerReady: () => call<boolean>("ci_docker_ready"),
};

// ---------- window controls (frameless window) ----------

async function withWindow(fn: (w: import("@tauri-apps/api/window").Window) => Promise<unknown>) {
  if (!isTauri()) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await fn(getCurrentWindow());
}

export const closeWindow = () => withWindow((w) => w.close());
export const minimizeWindow = () => withWindow((w) => w.minimize());
export const toggleMaximizeWindow = () => withWindow((w) => w.toggleMaximize());

export async function openExternal(url: string) {
  if (!isTauri()) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

export async function revealPath(path: string) {
  if (!isTauri()) return;
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

/** Native folder picker. Returns null if cancelled (or not running in Tauri). */
/** Native file picker. Returns null if cancelled (or not running in Tauri). */
export async function pickFile(title = "Select file", extensions?: string[]): Promise<string | null> {
  if (!isTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const res = await open({ directory: false, multiple: false, title, filters: extensions ? [{ name: "Scripts", extensions }] : undefined });
  return typeof res === "string" ? res : null;
}

export async function pickFolder(title = "Select folder"): Promise<string | null> {
  if (!isTauri()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const res = await open({ directory: true, multiple: false, title });
  return typeof res === "string" ? res : null;
}
