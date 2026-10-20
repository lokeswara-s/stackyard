export type Profile = "local" | "staging-like" | "prod-sim";
export const PROFILES: { id: Profile; label: string; hint: string }[] = [
  { id: "local", label: "Local", hint: "Plain local processes, dev env vars" },
  { id: "staging-like", label: "Staging-like", hint: "Staging endpoints and feature flags" },
  { id: "prod-sim", label: "Prod simulation", hint: "Production-like config, strict env" },
];

export interface EnvVar {
  key: string;
  value: string;
  secret: boolean;
  /** undefined = all profiles */
  profile?: string | null;
}

export interface Service {
  name: string;
  kind: "app" | "infra";
  stack: string;
  repoPath: string;
  command: string;
  /** Terminal the start command runs in: "" (default) | cmd | powershell | bash. */
  shell?: string;
  /** Skip the terminal's own profile (loaded by default, like a regular terminal). */
  noProfile?: boolean;
  /** Script sourced in the terminal right before the start command. */
  shellInit?: string;
  port?: number | null;
  dependsOn: string[];
  image?: string | null;
  containerPort?: number | null;
  dataPath?: string | null;
  env: EnvVar[];
  /** Route the port through the fault-injection proxy. */
  chaos?: boolean;
}

export interface Workspace {
  id: string;
  name: string;
  description: string;
  profile: Profile;
  syncBeforeUp: boolean;
  services: Service[];
  env: EnvVar[];
  /** Variables / secrets the CI simulator exposes to pipelines (GitLab variables, GitHub vars+secrets, Jenkins credentials). */
  ciVars?: EnvVar[];
  /** Tools this workspace needs installed (label + a command that exits 0 when present). */
  prerequisites?: Prerequisite[];
}

export interface Prerequisite {
  label: string;
  command: string;
  /** Optional text the output must contain (case-insensitive), e.g. "v24" to pin a version. */
  expected?: string;
}

export interface PrerequisiteResult {
  label: string;
  command: string;
  expected?: string;
  ok: boolean;
  /** Exit code, or null when the command could not run / timed out. */
  code?: number | null;
  /** Combined output (trimmed, capped). */
  output: string;
  error?: string | null;
  durationMs: number;
}

export interface Settings {
  healthTimeoutSecs: number;
  stopOnExit: boolean;
  /** CI runner images the user added on the Plugins page. */
  ciImages?: string[];
  /** Image for CI jobs that do not name one (empty = built-in default). */
  ciDefaultImage?: string;
}

export interface StoreData {
  workspaces: Workspace[];
  active?: string | null;
  settings: Settings;
}

export type RuntimeStatus = "idle" | "starting" | "running" | "stopping" | "stopped" | "failed";

export interface ServiceRuntime {
  name: string;
  status: RuntimeStatus;
  pid?: number | null;
  startedMs?: number | null;
  exitCode?: number | null;
  memMb?: number | null;
  cpu?: number | null;
  /** Accepting connections (on the configured port, or on one we detected). */
  portOpen: boolean;
  /** Ports the process tree is really listening on (detected from the OS). */
  ports?: number[];
  /** Port to show/open: the configured one when open, otherwise the first detected one. */
  livePort?: number | null;
  /** A port is configured but the service listens somewhere else. */
  portMismatch?: boolean;
  /** "degraded" = running but it logged errors in the last minute. */
  health?: "none" | "starting" | "healthy" | "degraded";
  /** Distinct errors since start, last minute only (a stack trace counts once). */
  errorCount?: number;
  lastError?: string | null;
  /** Plain-language explanation, e.g. which service a refused connection was aimed at. */
  healthHint?: string | null;
  /** off | armed (uses the fault proxy on next start) | proxy (routed, no faults) | faulting */
  chaos?: "off" | "armed" | "proxy" | "faulting";
}

// ---------- fault injection ----------

export interface Faults {
  latencyMs: number;
  jitterMs: number;
  /** % of requests answered with `status` (HTTP services). */
  errorRate: number;
  status: number;
  body: string;
  /** % of requests (HTTP) / connections and chunks (TCP) that are cut. */
  dropRate: number;
  blackhole: boolean;
  /** KB/s, 0 = unlimited */
  throttleKbps: number;
  pathPrefix: string;
}

export const NO_FAULTS: Faults = { latencyMs: 0, jitterMs: 0, errorRate: 0, status: 503, body: "", dropRate: 0, blackhole: false, throttleKbps: 0, pathPrefix: "" };

export interface ChaosEvent {
  seq: number;
  ts: number;
  method: string;
  path: string;
  outcome: "forwarded" | "error" | "dropped" | "held" | "upstream-down" | "passthrough" | "connection";
  status?: number | null;
  ms: number;
  addedMs: number;
}

export interface ChaosStats {
  requests: number;
  forwarded: number;
  injectedErrors: number;
  dropped: number;
  held: number;
  upstreamDown: number;
  addedLatencyMs: number;
  rps: number;
  recent: ChaosEvent[];
}

export interface ChaosState {
  service: string;
  kind: "app" | "infra";
  mode: "http" | "tcp";
  enabled: boolean;
  active: boolean;
  publicPort?: number | null;
  upstreamPort?: number | null;
  faults: Faults;
  stats?: ChaosStats | null;
}

export interface ProbeResult {
  status?: number | null;
  ms: number;
  error?: string | null;
  body: string;
}

export interface AppliedCommand {
  command: string;
  notes: string[];
}

export interface LogLine {
  seq: number;
  ts: number;
  workspace: string;
  service: string;
  stream: "stdout" | "stderr" | "system";
  level: "error" | "warn" | "info" | "debug";
  text: string;
}

export interface FileChange {
  path: string;
  index: string;
  worktree: string;
  added?: number | null;
  removed?: number | null;
}

export interface StorageItem {
  id: string;
  label: string;
  description: string;
  bytes?: number | null;
  sizeText?: string | null;
  count?: number | null;
  danger: boolean;
  dockerWide: boolean;
  routine: boolean;
  blocked?: string | null;
}

export interface StorageReport {
  dataDir: string;
  dataBytes: number;
  diskFreeBytes?: number | null;
  minFreeBytes: number;
  docker: boolean;
  items: StorageItem[];
}

export interface CleanResult {
  freedBytes: number;
  messages: string[];
}

export interface ShellInfo {
  id: string;
  label: string;
  available: boolean;
  path?: string | null;
  /** Where this terminal's own profile lives (what "load my profile" loads). */
  profile?: string | null;
}

export interface StashEntry {
  index: number;
  message: string;
  age: string;
}

export interface BranchInfo {
  name: string;
  upstream?: string | null;
  lastCommit: string;
  current: boolean;
}

export interface Branches {
  current: string;
  local: BranchInfo[];
  /** Remote branches without a local branch yet, e.g. `origin/feature-x`. */
  remote: BranchInfo[];
}

export interface GitStatus {
  path: string;
  exists: boolean;
  isRepo: boolean;
  error?: string | null;
  branch: string;
  upstream?: string | null;
  ahead: number;
  behind: number;
  changes: FileChange[];
  untracked: string[];
  lastCommit: string;
  lastFetchMs?: number | null;
  remote?: string | null;
}

export type SyncOutcome = "synced" | "upToDate" | "skippedDirty" | "diverged" | "noUpstream" | "notRepo" | "error";
export interface SyncResult {
  path: string;
  outcome: SyncOutcome;
  message: string;
}

export interface Conflict {
  service: string;
  port: number;
  pid: number;
  process: string;
  /** Docker container publishing the port, if the port is held by a container. */
  container?: string | null;
}

export type StepStatus = "pending" | "running" | "passed" | "warn" | "failed" | "skipped";
export interface StageItem {
  name: string;
  status: StepStatus;
  detail: string;
}
export interface Stage {
  id: string;
  title: string;
  status: StepStatus;
  detail: string;
  items: StageItem[];
  startedMs?: number | null;
  finishedMs?: number | null;
}
export interface UpRun {
  workspaceId: string;
  workspaceName: string;
  profile: string;
  startedMs: number;
  finishedMs?: number | null;
  status: "running" | "success" | "failed" | "cancelled";
  stages: Stage[];
  conflict?: Conflict | null;
  error?: string | null;
}

export interface Listener {
  port: number;
  pid: number;
  process: string;
  container?: string | null;
}

export interface DockerOverview {
  available: boolean;
  version?: string | null;
  error?: string | null;
  containers: Record<string, string>[];
  volumes: Record<string, string>[];
  images: Record<string, string>[];
}

export interface Detected {
  path: string;
  name: string;
  stack: string;
  command: string;
  port?: number | null;
  isGit: boolean;
  notes: string[];
}

export interface SystemStats {
  cpu: number;
  cores: number;
  memUsedGb: number;
  memTotalGb: number;
}

// ---------- CI simulator ----------

export type CiProvider = "gitlab" | "github" | "jenkins";

export interface CiKv {
  key: string;
  value: string;
}

export interface CiPipelineFile {
  provider: CiProvider;
  file: string;
}

export interface CiRepoPipelines {
  repoPath: string;
  pipelines: CiPipelineFile[];
}

export interface CiParam {
  name: string;
  default: string;
  description: string;
  kind: "string" | "boolean" | "choice" | string;
  choices: string[];
}

export interface CiJobDef {
  id: string;
  name: string;
  stage: string;
  needs: { job: string; artifacts: boolean; optional: boolean }[];
  hasNeeds: boolean;
  image?: string | null;
  services: { image: string; alias: string }[];
  steps: { name: string; kind?: { type: string } }[];
  when: string;
  allowFailure: boolean;
  retry: number;
  runsOn?: string | null;
  matrix: CiKv[];
}

export interface CiPipeline {
  provider: CiProvider;
  file: string;
  name: string;
  stages: string[];
  jobs: CiJobDef[];
  env: CiKv[];
  warnings: string[];
  gatedByStage: boolean;
  params: CiParam[];
  events: string[];
  defaultImage?: string | null;
}

export interface CiRefs {
  branches: string[];
  tags: string[];
  current: string;
}

export interface CiRunOptions {
  provider: CiProvider;
  file: string;
  repoPath: string;
  event: string;
  gitRef: string;
  /** committed = clean checkout of the commit (like a server) | working = include uncommitted changes */
  source: "committed" | "working";
  runner: "auto" | "docker" | "host";
  onlyJobs: string[];
  variables: CiKv[];
  /** Image for jobs that do not name one (GitLab/Jenkins), or the runner image for `ubuntu-latest` (GitHub). Empty = default. */
  ubuntuImage?: string;
  /** With a job selection: also run the jobs the picked ones need. */
  withNeeds?: boolean;
  /** With exactly one picked job: run only these steps (indexes). Empty = all. */
  onlySteps?: number[];
}

export type CiStatus = "pending" | "running" | "passed" | "failed" | "warning" | "skipped" | "canceled" | "manual";

export interface CiStepView {
  name: string;
  kind: string;
  status: string;
  startedMs?: number | null;
  finishedMs?: number | null;
}

export interface CiJobView {
  id: string;
  name: string;
  stage: string;
  status: CiStatus;
  needs: string[];
  startedMs?: number | null;
  finishedMs?: number | null;
  attempt: number;
  note?: string | null;
  allowFailure: boolean;
  steps: CiStepView[];
  artifacts: string[];
  runner: string;
  logLines: number;
  coverage?: string | null;
}

export interface CiTrigger {
  branch: string;
  tag: string;
  refName: string;
  sha: string;
  shortSha: string;
  message: string;
  title: string;
  authorName: string;
  event: string;
  working: boolean;
  projectName: string;
}

export interface CiRunView {
  id: string;
  number: number;
  provider: CiProvider;
  file: string;
  name: string;
  repoPath: string;
  status: CiStatus | "blocked";
  startedMs: number;
  finishedMs?: number | null;
  trigger: CiTrigger;
  source: string;
  runner: string;
  stages: string[];
  jobs: CiJobView[];
  warnings: string[];
  notes: string[];
  dir: string;
}

export interface CiLogLine {
  step: number;
  ts: number;
  text: string;
}

export interface CiLogPage {
  lines: CiLogLine[];
  next: number;
}
