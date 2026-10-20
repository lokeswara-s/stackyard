import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Banner, Btn, Card, ConfirmBtn, EmptyState, Field, Icon, PageBody, PageHeader, inputCls, timeAgo } from "../components/ui";
import { api } from "../lib/tauri";
import type { CiJobView, CiKv, CiLogLine, CiPipeline, CiPipelineFile, CiProvider, CiRefs, CiRepoPipelines, CiRunOptions, CiRunView, EnvVar } from "../lib/types";
import { useApp } from "../state/AppState";

// ---------------------------------------------------------------- helpers

const PROVIDER: Record<CiProvider, { label: string; icon: string; cls: string; events: { id: string; label: string }[] }> = {
  gitlab: {
    label: "GitLab CI",
    icon: "rocket_launch",
    cls: "bg-tertiary/15 text-tertiary border-tertiary/40",
    events: [
      { id: "push", label: "Push" },
      { id: "merge_request_event", label: "Merge request" },
      { id: "web", label: "Run pipeline (web)" },
      { id: "schedule", label: "Schedule" },
    ],
  },
  github: {
    label: "GitHub Actions",
    icon: "play_circle",
    cls: "bg-primary/10 text-primary border-primary/30",
    events: [
      { id: "push", label: "push" },
      { id: "pull_request", label: "pull_request" },
      { id: "workflow_dispatch", label: "workflow_dispatch" },
      { id: "schedule", label: "schedule" },
    ],
  },
  jenkins: {
    label: "Jenkins",
    icon: "construction",
    cls: "bg-secondary/10 text-secondary border-secondary/30",
    events: [
      { id: "push", label: "Branch build" },
      { id: "pull_request", label: "Pull request" },
    ],
  },
};

const STATUS: Record<string, { icon: string; cls: string; label: string }> = {
  pending: { icon: "radio_button_unchecked", cls: "text-outline", label: "Pending" },
  running: { icon: "progress_activity", cls: "text-primary animate-spin", label: "Running" },
  passed: { icon: "check_circle", cls: "text-secondary", label: "Passed" },
  failed: { icon: "error", cls: "text-error", label: "Failed" },
  warning: { icon: "warning", cls: "text-tertiary", label: "Passed with warnings" },
  skipped: { icon: "remove_circle_outline", cls: "text-outline", label: "Skipped" },
  canceled: { icon: "cancel", cls: "text-outline", label: "Canceled" },
  manual: { icon: "play_circle", cls: "text-tertiary", label: "Manual" },
  blocked: { icon: "block", cls: "text-error", label: "Blocked" },
};

const stat = (s: string) => STATUS[s] ?? STATUS.pending;

function StatusIcon({ status, size = 18 }: { status: string; size?: number }) {
  const s = stat(status);
  const px = size <= 14 ? "text-[14px]" : size <= 16 ? "text-[16px]" : "text-[18px]";
  return <Icon name={s.icon} className={`${s.cls} ${px}`} />;
}

function dur(start?: number | null, end?: number | null, now = Date.now()): string {
  if (!start) return "";
  const s = Math.max(0, Math.round(((end ?? now) - start) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

const repoName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
const ANSI = /\u001b\[[0-9;?]*[A-Za-z]/g;
const isActive = (s: string) => s === "running";
const finalStatus = (s: string) => !["pending", "running"].includes(s);

function ProviderBadge({ p }: { p: CiProvider }) {
  const v = PROVIDER[p];
  return (
    <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded border text-[10px] font-label-sm ${v.cls}`}>
      <Icon name={v.icon} className="text-[12px]" />
      {v.label}
    </span>
  );
}

/** Columns of the job graph: the declared stages, or (single stage, e.g. GitHub) the depth in the `needs` graph. */
function columns<T extends { id: string; stage: string }>(jobs: T[], stages: string[], needsOf: (j: T) => string[]): { title: string; jobs: T[] }[] {
  const names = [...new Set(jobs.map((j) => j.stage))];
  if (stages.length > 1 || names.length > 1) {
    const order = stages.length ? stages : names;
    return order.map((st) => ({ title: st, jobs: jobs.filter((j) => j.stage === st) })).filter((c) => c.jobs.length > 0);
  }
  const base = (id: string) => id.replace(/ [([].*$/, "");
  const depth = new Map<string, number>();
  const calc = (j: T, seen: string[] = []): number => {
    if (depth.has(j.id)) return depth.get(j.id)!;
    if (seen.includes(j.id)) return 0;
    let d = 0;
    for (const n of needsOf(j)) for (const p of jobs.filter((x) => x.id === n || base(x.id) === base(n))) d = Math.max(d, calc(p, [...seen, j.id]) + 1);
    depth.set(j.id, d);
    return d;
  };
  jobs.forEach((j) => calc(j));
  const max = Math.max(0, ...depth.values());
  return Array.from({ length: max + 1 }, (_, i) => ({ title: max === 0 ? "jobs" : i === 0 ? "first" : `then · ${i}`, jobs: jobs.filter((j) => depth.get(j.id) === i) })).filter((c) => c.jobs.length > 0);
}

// ---------------------------------------------------------------- run form

interface Selection {
  repoPath: string;
  file: CiPipelineFile;
}

function RunForm({ sel, pipeline, onStart, docker }: { sel: Selection; pipeline: CiPipeline; onStart: (o: CiRunOptions) => Promise<void>; docker: boolean | null }) {
  const [refs, setRefs] = useState<CiRefs | null>(null);
  const [gitRef, setGitRef] = useState("");
  const [event, setEvent] = useState("push");
  const [source, setSource] = useState<"committed" | "working">("committed");
  const [runner, setRunner] = useState<"auto" | "docker" | "host">("auto");
  const [params, setParams] = useState<Record<string, string>>({});
  const [vars, setVars] = useState<CiKv[]>([]);
  const [only, setOnly] = useState<string[]>([]);
  const [image, setImage] = useState("");
  const [withNeeds, setWithNeeds] = useState(false);
  const [onlySteps, setOnlySteps] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);
  const { store } = useApp();
  const defaultImage = store.settings.ciDefaultImage?.trim() || "catthehacker/ubuntu:act-latest";
  const imageChoices = useMemo(() => [...new Set([defaultImage, ...(store.settings.ciImages ?? [])])], [defaultImage, store.settings.ciImages]);

  useEffect(() => {
    let stop = false;
    api.ciRefs(sel.repoPath).then((r) => {
      if (stop) return;
      setRefs(r);
      setGitRef(r.current);
    }).catch(() => undefined);
    return () => {
      stop = true;
    };
  }, [sel.repoPath]);

  useEffect(() => {
    setParams(Object.fromEntries(pipeline.params.map((p) => [p.name, p.default])));
    setOnly([]);
    const evs = PROVIDER[sel.file.provider].events;
    setEvent(evs[0].id);
  }, [pipeline, sel.file.provider]);

  const toggle = (id: string) => {
    setOnlySteps([]);
    setOnly((o) => (o.includes(id) ? o.filter((x) => x !== id) : [...o, id]));
  };
  const single = only.length === 1 ? pipeline.jobs.find((j) => j.id === only[0]) : undefined;
  const toggleStep = (i: number) => setOnlySteps((s) => (s.includes(i) ? s.filter((x) => x !== i) : [...s, i].sort((a, b) => a - b)));
  const cols = useMemo(() => columns(pipeline.jobs, pipeline.stages, (j) => j.needs.map((n) => n.job)), [pipeline]);

  const start = async () => {
    setBusy(true);
    try {
      await onStart({
        provider: sel.file.provider,
        file: sel.file.file,
        repoPath: sel.repoPath,
        event,
        gitRef,
        source,
        runner,
        onlyJobs: only,
        withNeeds: only.length > 0 && withNeeds,
        onlySteps: single ? onlySteps : [],
        ubuntuImage: image.trim(),
        variables: [...Object.entries(params).map(([key, value]) => ({ key, value })), ...vars.filter((v) => v.key.trim())],
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-space-md">
      <Card title="Pipeline" icon="account_tree" actions={
          <div className="flex items-center gap-2 text-[11px] text-outline">
            <span>click jobs to run only those (rules and conditions are ignored for picked jobs)</span>
            {only.length > 0 && (
              <Btn variant="ghost" onClick={() => { setOnly([]); setOnlySteps([]); }}>
                Clear
              </Btn>
            )}
          </div>
        } bodyClass="p-space-md overflow-x-auto">
        <div className="flex gap-space-lg min-w-max">
          {cols.map((col) => (
            <div key={col.title} className="w-52 shrink-0">
              <div className="text-[10px] font-label-sm uppercase tracking-wider text-outline mb-1.5">{col.title}</div>
              <div className="space-y-1.5">
                {col.jobs.map((j) => {
                    const on = only.includes(j.id);
                    return (
                      <button
                        key={j.id}
                        type="button"
                        onClick={() => toggle(j.id)}
                        className={`w-full text-left px-2.5 py-1.5 rounded-lg border ${on ? "border-primary bg-primary/10" : "border-surface-container-highest bg-surface-container-lowest hover:border-outline"}`}
                      >
                        <div className="font-label-md text-label-md text-on-surface truncate" title={j.id}>
                          {j.name}
                        </div>
                        <div className="text-[10px] text-outline truncate">
                          {j.image ?? j.runsOn ?? (pipeline.defaultImage ? pipeline.defaultImage : "this machine")}
                          {j.when === "manual" && " · manual"}
                          {j.services.length > 0 && ` · +${j.services.length} service${j.services.length > 1 ? "s" : ""}`}
                        </div>
                      </button>
                    );
                  })}
              </div>
            </div>
          ))}
        </div>
      </Card>

      <Card title="Run" icon="play_arrow">
        <div className="grid grid-cols-4 gap-space-md">
          <Field label="Branch / tag">
            <select className={inputCls} value={gitRef} onChange={(e) => setGitRef(e.target.value)}>
              {refs && refs.branches.length === 0 && refs.tags.length === 0 && <option value="">(no commits)</option>}
              {refs?.branches.length ? (
                <optgroup label="Branches">
                  {refs.branches.map((b) => (
                    <option key={b} value={b}>
                      {b}
                    </option>
                  ))}
                </optgroup>
              ) : null}
              {refs?.tags.length ? (
                <optgroup label="Tags">
                  {refs.tags.map((b) => (
                    <option key={b} value={b}>
                      {b}
                    </option>
                  ))}
                </optgroup>
              ) : null}
            </select>
          </Field>
          <Field label="Triggered by">
            <select className={inputCls} value={event} onChange={(e) => setEvent(e.target.value)}>
              {PROVIDER[sel.file.provider].events.map((e) => (
                <option key={e.id} value={e.id}>
                  {e.label}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Code" hint={source === "committed" ? "Clean checkout of the commit, like a server" : "Your working tree, including uncommitted changes"}>
            <select className={inputCls} value={source} onChange={(e) => setSource(e.target.value as "committed" | "working")}>
              <option value="committed">Committed code (server-like)</option>
              <option value="working">Working tree (with my changes)</option>
            </select>
          </Field>
          <Field label="Runs in" hint={docker === false ? "Docker is not running: start Docker Desktop, or jobs fall back to this machine (Git Bash, missing tools)" : undefined}>
            <select className={inputCls} value={runner} onChange={(e) => setRunner(e.target.value as "auto" | "docker" | "host")}>
              <option value="auto">Auto (containers when Docker is up)</option>
              <option value="docker">Docker containers only</option>
              <option value="host">This machine (no containers)</option>
            </select>
          </Field>
        </div>

        <div className="mt-space-md grid grid-cols-2 gap-space-md">
          <Field
            label={sel.file.provider === "github" ? "Runner image for ubuntu-latest" : "Image for jobs that do not name one"}
            hint="Empty = the default runner image from Plugins (pulled once, then reused). Pick another to match your server's runner."
          >
            <input className={`${inputCls} font-code-dense`} list="ci-images" value={image} onChange={(e) => setImage(e.target.value)} placeholder={defaultImage} />
            <datalist id="ci-images">
              {imageChoices.map((i) => (
                <option key={i} value={i} />
              ))}
            </datalist>
          </Field>
        </div>

        {only.length > 0 && (
          <div className="mt-space-md pt-space-md border-t border-surface-container-highest">
            <label className="flex items-center gap-2 text-[12px] text-on-surface-variant">
              <input type="checkbox" checked={withNeeds} onChange={(e) => setWithNeeds(e.target.checked)} />
              Also run the jobs the picked ones need (earlier stages / <span className="font-code-dense">needs</span>), so artifacts and outputs exist
            </label>
            {single && single.steps.length > 0 && (
              <div className="mt-space-sm">
                <div className="flex items-center justify-between mb-1">
                  <span className="font-label-sm text-label-sm text-on-surface-variant">Steps of {single.name}: run only the ticked ones{onlySteps.length === 0 && " (none ticked = all)"}</span>
                  {onlySteps.length > 0 && (
                    <Btn variant="ghost" onClick={() => setOnlySteps([])}>
                      All steps
                    </Btn>
                  )}
                </div>
                <ul className="grid grid-cols-2 gap-x-space-md gap-y-0.5">
                  {single.steps.map((s, i) => (
                    <li key={i}>
                      <label className="flex items-center gap-2 text-[12px] text-on-surface-variant cursor-pointer">
                        <input type="checkbox" checked={onlySteps.includes(i)} onChange={() => toggleStep(i)} />
                        <span className="truncate" title={s.name}>
                          {i + 1}. {s.name}
                        </span>
                      </label>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}

        {pipeline.params.length > 0 && (
          <div className="mt-space-md pt-space-md border-t border-surface-container-highest">
            <div className="font-label-sm text-label-sm text-on-surface-variant mb-1.5">{sel.file.provider === "github" ? "Workflow inputs" : "Parameters"}</div>
            <div className="grid grid-cols-3 gap-space-md">
              {pipeline.params.map((p) => (
                <Field key={p.name} label={p.name} hint={p.description || undefined}>
                  {p.kind === "boolean" ? (
                    <select className={inputCls} value={params[p.name] ?? "false"} onChange={(e) => setParams({ ...params, [p.name]: e.target.value })}>
                      <option value="false">false</option>
                      <option value="true">true</option>
                    </select>
                  ) : p.choices.length > 0 ? (
                    <select className={inputCls} value={params[p.name] ?? ""} onChange={(e) => setParams({ ...params, [p.name]: e.target.value })}>
                      {p.choices.map((c) => (
                        <option key={c}>{c}</option>
                      ))}
                    </select>
                  ) : (
                    <input className={`${inputCls} font-code-dense`} value={params[p.name] ?? ""} onChange={(e) => setParams({ ...params, [p.name]: e.target.value })} />
                  )}
                </Field>
              ))}
            </div>
          </div>
        )}

        <div className="mt-space-md pt-space-md border-t border-surface-container-highest">
          <div className="flex items-center justify-between mb-1.5">
            <span className="font-label-sm text-label-sm text-on-surface-variant">Extra variables for this run</span>
            <Btn variant="ghost" icon="add" onClick={() => setVars([...vars, { key: "", value: "" }])}>
              Add
            </Btn>
          </div>
          {vars.map((v, i) => (
            <div key={i} className="flex gap-2 mb-1.5">
              <input className={`${inputCls} !w-56 font-code-dense`} placeholder="NAME" value={v.key} onChange={(e) => setVars(vars.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))} />
              <input className={`${inputCls} font-code-dense`} placeholder="value" value={v.value} onChange={(e) => setVars(vars.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))} />
              <Btn variant="ghost" icon="close" onClick={() => setVars(vars.filter((_, j) => j !== i))} aria-label="Remove" />
            </div>
          ))}
        </div>

        <div className="flex items-center justify-between mt-space-md">
          <span className="text-[11px] text-outline">{only.length ? `Only ${only.length} picked job(s)${withNeeds ? " and what they need" : ""}${single && onlySteps.length ? `, ${onlySteps.length} step(s)` : ""}` : `All ${pipeline.jobs.length} jobs, as the server would decide`}</span>
          <Btn variant="primary" icon="play_arrow" disabled={busy} onClick={() => void start()}>
            {busy ? "Starting…" : "Run pipeline"}
          </Btn>
        </div>
      </Card>
    </div>
  );
}

// ---------------------------------------------------------------- log viewer

function lineCls(t: string): string {
  if (t.startsWith("$ ") || t.startsWith("+ ") || t.startsWith("[Pipeline]") || t.startsWith("Run ")) return "text-tertiary";
  if (/^(ERROR|Error|##\[error\]|fatal:)/.test(t) || /Job failed|Process completed with exit code/.test(t)) return "text-error";
  if (/^(Job succeeded|Preparing|Job ready|runner:|Checking out|Uploading|Uploaded|Restoring|Saving)/.test(t)) return "text-primary";
  if (/^(WARNING|Warning|##\[warning\])/.test(t)) return "text-tertiary";
  return "text-on-surface";
}

function LogView({ runId, job, stepNames, live }: { runId: string; job: string; stepNames: string[]; live: boolean }) {
  const [lines, setLines] = useState<CiLogLine[]>([]);
  const next = useRef(0);
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const liveRef = useRef(live);
  liveRef.current = live;

  useEffect(() => {
    setLines([]);
    next.current = 0;
    stick.current = true;
    let stop = false;
    let idle = 0;
    const tick = async () => {
      try {
        const page = await api.ciLog(runId, job, next.current);
        if (stop || !page) return;
        if (page.lines.length) {
          idle = 0;
          next.current = page.next;
          setLines((l) => l.concat(page.lines));
        } else idle++;
      } catch {
        /* ignore */
      }
    };
    void tick();
    const t = setInterval(() => {
      if (!liveRef.current && idle > 2) return;
      void tick();
    }, 800);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [runId, job]);

  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  let lastStep = -2;
  return (
    <div
      ref={box}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
      className="h-[420px] overflow-auto bg-surface-container-lowest border border-surface-container-highest rounded-lg p-space-sm font-code-dense text-code-dense select-text"
    >
      {lines.length === 0 && <div className="text-outline">{live ? "Waiting for output…" : "No output."}</div>}
      {lines.map((l, i) => {
        const head = l.step !== lastStep && l.step >= 0 && stepNames[l.step];
        lastStep = l.step;
        return (
          <div key={i}>
            {head && <div className="mt-2 mb-0.5 pt-1 border-t border-surface-container-highest text-[10px] uppercase tracking-wider text-outline">{stepNames[l.step]}</div>}
            <div className={`whitespace-pre-wrap break-all ${lineCls(l.text.replace(ANSI, ""))}`}>{l.text.replace(ANSI, "") || " "}</div>
          </div>
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------- run view

function RunPanel({ run, onRerun, onDeleted, toast }: { run: CiRunView; onRerun: () => void; onDeleted: () => void; toast: (m: string, k?: "success" | "error" | "info") => void }) {
  const [jobId, setJobId] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());
  const running = run.status === "running";

  useEffect(() => {
    if (!running) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [running]);

  // Follow the first running/failed job until the user picks one.
  const picked = useRef(false);
  useEffect(() => {
    picked.current = false;
    setJobId(null);
  }, [run.id]);
  useEffect(() => {
    if (picked.current && run.jobs.some((j) => j.id === jobId)) return;
    const cand = run.jobs.find((j) => j.status === "running") ?? run.jobs.find((j) => j.status === "failed") ?? [...run.jobs].reverse().find((j) => finalStatus(j.status) && j.status !== "skipped") ?? run.jobs[0];
    if (cand && cand.id !== jobId) setJobId(cand.id);
  }, [run.jobs, jobId]);

  const job: CiJobView | undefined = run.jobs.find((j) => j.id === jobId);
  const cols = useMemo(() => columns(run.jobs, run.stages, (j) => j.needs), [run.jobs, run.stages]);
  const t = run.trigger;

  const act = async (fn: () => Promise<void>, ok?: string) => {
    try {
      await fn();
      if (ok) toast(ok, "success");
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  };

  return (
    <div className="space-y-space-md min-w-0">
      <Card
        title={
          <span className="flex items-center gap-2">
            <StatusIcon status={run.status} />
            {run.name || run.file} <span className="text-outline font-code-dense">#{run.number}</span>
            <span className="text-[11px] font-body-sm text-on-surface-variant">{stat(run.status).label}</span>
          </span>
        }
        icon={undefined}
        actions={
          <div className="flex items-center gap-1.5">
            {running ? (
              <Btn variant="danger" icon="stop" onClick={() => void act(() => api.ciCancel(run.id))}>
                Cancel
              </Btn>
            ) : (
              <>
                <Btn icon="replay" onClick={onRerun}>
                  Run again
                </Btn>
                <ConfirmBtn icon="delete" onConfirm={() => void act(() => api.ciDelete(run.id).then(onDeleted))} confirmLabel="Delete run?">
                  Delete
                </ConfirmBtn>
              </>
            )}
          </div>
        }
      >
        <div className="flex flex-wrap items-center gap-x-space-lg gap-y-1 text-[12px] text-on-surface-variant">
          <span className="inline-flex items-center gap-1">
            <Icon name="fork_right" className="text-[14px]" />
            {t.tag ? `tag ${t.tag}` : t.branch || t.refName}
            <span className="font-code-dense text-outline">@{t.shortSha}</span>
          </span>
          <span className="truncate max-w-[420px]" title={t.message}>
            {t.title}
          </span>
          <span>{t.authorName}</span>
          <span className="inline-flex items-center gap-1">
            <Icon name="schedule" className="text-[14px]" />
            {dur(run.startedMs, run.finishedMs, now)}
          </span>
          <span>{run.source === "working" || t.working ? "working tree" : "committed code"}</span>
          <span>{t.event}</span>
        </div>
        {run.notes.map((n, i) => (
          <div key={i} className="mt-1.5 text-[12px] text-tertiary flex gap-1.5">
            <Icon name="info" className="text-[14px] mt-px" />
            {n}
          </div>
        ))}
        {run.warnings.map((n, i) => (
          <div key={i} className="mt-1 text-[12px] text-on-surface-variant flex gap-1.5">
            <Icon name="warning" className="text-[14px] mt-px text-tertiary" />
            {n}
          </div>
        ))}
      </Card>

      <Card title="Jobs" icon="account_tree" bodyClass="p-space-md overflow-x-auto">
        <div className="flex gap-space-lg min-w-max">
          {cols.map((col) => {
            return (
              <div key={col.title} className="w-56 shrink-0">
                <div className="text-[10px] font-label-sm uppercase tracking-wider text-outline mb-1.5">{col.title}</div>
                <div className="space-y-1.5">
                  {col.jobs.map((j) => (
                    <div
                      key={j.id}
                      className={`rounded-lg border px-2.5 py-1.5 cursor-pointer ${jobId === j.id ? "border-primary bg-primary/10" : "border-surface-container-highest bg-surface-container-lowest hover:border-outline"}`}
                      onClick={() => {
                        picked.current = true;
                        setJobId(j.id);
                      }}
                    >
                      <div className="flex items-center gap-1.5">
                        <StatusIcon status={j.status} size={16} />
                        <span className="font-label-md text-label-md text-on-surface truncate flex-1" title={j.id}>
                          {j.name}
                        </span>
                        {!running && finalStatus(j.status) && (
                          <button
                            type="button"
                            title={j.status === "manual" ? "Run this manual job" : j.status === "skipped" ? "Run this job anyway" : "Run this job again"}
                            className="text-tertiary hover:text-primary"
                            onClick={(e) => {
                              e.stopPropagation();
                              void act(() => api.ciPlay(run.id, j.id));
                            }}
                          >
                            <Icon name="play_arrow" className="text-[18px]" />
                          </button>
                        )}
                      </div>
                      <div className="flex justify-between text-[10px] text-outline mt-0.5">
                        <span className="truncate">{j.note ?? (j.attempt > 1 ? `attempt ${j.attempt}` : stat(j.status).label)}</span>
                        <span className="font-code-dense">{dur(j.startedMs, j.finishedMs, now)}</span>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </Card>

      {job && (
        <Card
          title={
            <span className="flex items-center gap-2">
              <StatusIcon status={job.status} size={16} />
              {job.name}
              <span className="text-[11px] font-body-sm text-outline">{job.runner}</span>
            </span>
          }
          icon={undefined}
          bodyClass="p-space-md"
          actions={
            <div className="flex items-center gap-2">
              {job.coverage && <span className="text-[11px] text-secondary">coverage {job.coverage}</span>}
              {!running && finalStatus(job.status) && (
                <Btn icon="play_arrow" onClick={() => void act(() => api.ciPlay(run.id, job.id))}>
                  {job.status === "skipped" || job.status === "manual" ? "Run this job" : "Run again"}
                </Btn>
              )}
            </div>
          }
        >
          <div className="grid gap-space-md" style={{ gridTemplateColumns: "240px minmax(0,1fr)" }}>
            <div className="space-y-space-md min-w-0">
              <div>
                <div className="text-[10px] font-label-sm uppercase tracking-wider text-outline mb-1">Steps</div>
                <ul className="space-y-0.5">
                  {job.steps.map((s, i) => (
                    <li key={i} className="flex items-center gap-1.5 text-[12px]">
                      <StatusIcon status={s.status} size={14} />
                      <span className="truncate flex-1 text-on-surface-variant" title={s.name}>
                        {s.name}
                      </span>
                      <span className="font-code-dense text-outline text-[10px]">{dur(s.startedMs, s.finishedMs, now)}</span>
                      {!running && finalStatus(job.status) && (
                        <button type="button" title="Run only this step (a fresh container; earlier steps are not repeated)" className="text-outline hover:text-primary" onClick={() => void act(() => api.ciPlay(run.id, job.id, [i]))}>
                          <Icon name="play_arrow" className="text-[14px]" />
                        </button>
                      )}
                    </li>
                  ))}
                  {job.steps.length === 0 && <li className="text-[12px] text-outline">Nothing ran.</li>}
                </ul>
              </div>
              {job.artifacts.length > 0 && (
                <div>
                  <div className="text-[10px] font-label-sm uppercase tracking-wider text-outline mb-1">Artifacts</div>
                  <ul className="space-y-0.5 max-h-40 overflow-auto">
                    {job.artifacts.map((a) => (
                      <li key={a} className="font-code-dense text-[11px] text-on-surface-variant truncate" title={a}>
                        {a}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
            {job.status === "skipped" || (job.status === "manual" && job.logLines === 0) ? (
              <div className="rounded-lg border border-dashed border-outline-variant p-space-lg text-center text-on-surface-variant font-body-sm self-start">
                {job.status === "manual" ? "This job waits for you: press “Run this job”." : `${job.note ?? "This job did not run."} You can still run it with “Run this job”.`}
              </div>
            ) : (
              <LogView runId={run.id} job={job.id} stepNames={job.steps.map((s) => s.name)} live={isActive(job.status) || running} />
            )}
          </div>
        </Card>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- CI variables

function VarsCard() {
  const { active, saveWorkspace } = useApp();
  const [rows, setRows] = useState<EnvVar[]>(active?.ciVars ?? []);
  const [dirty, setDirty] = useState(false);
  const wsId = active?.id;
  useEffect(() => {
    setRows(active?.ciVars ?? []);
    setDirty(false);
    // only when the workspace changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wsId]);
  if (!active) return null;
  const set = (r: EnvVar[]) => {
    setRows(r);
    setDirty(true);
  };
  return (
    <Card
      title="CI variables & secrets"
      icon="vpn_key"
      actions={
        <div className="flex gap-1.5">
          <Btn variant="ghost" icon="add" onClick={() => set([...rows, { key: "", value: "", secret: false }])}>
            Add
          </Btn>
          <Btn variant="primary" icon="save" disabled={!dirty} onClick={() => void saveWorkspace({ ...active, ciVars: rows.filter((r) => r.key.trim()) }).then(() => setDirty(false))}>
            Save
          </Btn>
        </div>
      }
    >
      <p className="text-[12px] text-on-surface-variant mb-space-sm">
        What you would configure on the server: <b>GitLab</b> CI/CD variables (secret ones are masked in logs), <b>GitHub</b> <code className="font-code-dense">vars.*</code> (plain) and{" "}
        <code className="font-code-dense">secrets.*</code> (secret), <b>Jenkins</b> credentials by id (<code className="font-code-dense">credentials('id')</code>; for username/password use{" "}
        <code className="font-code-dense">id_USR</code> and <code className="font-code-dense">id_PSW</code>). They stay on this machine.
      </p>
      {rows.length === 0 && <div className="text-[12px] text-outline">None yet.</div>}
      {rows.map((r, i) => (
        <div key={i} className="flex gap-2 mb-1.5 items-center">
          <input className={`${inputCls} !w-64 font-code-dense`} placeholder="NAME" value={r.key} onChange={(e) => set(rows.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))} />
          <input
            className={`${inputCls} font-code-dense`}
            type={r.secret ? "password" : "text"}
            placeholder="value"
            value={r.value}
            onChange={(e) => set(rows.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))}
          />
          <label className="inline-flex items-center gap-1 text-[12px] text-on-surface-variant shrink-0">
            <input type="checkbox" checked={r.secret} onChange={(e) => set(rows.map((x, j) => (j === i ? { ...x, secret: e.target.checked } : x)))} className="accent-primary" />
            secret
          </label>
          <Btn variant="ghost" icon="close" onClick={() => set(rows.filter((_, j) => j !== i))} aria-label="Remove" />
        </div>
      ))}
    </Card>
  );
}

// ---------------------------------------------------------------- page

export default function PipelinesPage() {
  const { active, toast, ready } = useApp();
  const navigate = useNavigate();
  const [repos, setRepos] = useState<CiRepoPipelines[] | null>(null);
  const [sel, setSel] = useState<Selection | null>(null);
  const [pipeline, setPipeline] = useState<CiPipeline | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [runs, setRuns] = useState<CiRunView[]>([]);
  const [runId, setRunId] = useState<string | null>(null);
  const [docker, setDocker] = useState<boolean | null>(null);
  const [showVars, setShowVars] = useState(false);
  const lastOpts = useRef<Record<string, CiRunOptions>>({});
  const wsId = active?.id;

  const scan = useCallback(async () => {
    if (!wsId) return;
    try {
      const r = await api.ciPipelines(wsId);
      setRepos(r);
      setSel((cur) => {
        if (cur && r.some((x) => x.repoPath === cur.repoPath && x.pipelines.some((p) => p.file === cur.file.file))) return cur;
        const first = r.find((x) => x.pipelines.length);
        return first ? { repoPath: first.repoPath, file: first.pipelines[0] } : null;
      });
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  }, [wsId, toast]);

  useEffect(() => {
    void scan();
    api.ciDockerReady().then(setDocker).catch(() => setDocker(null));
  }, [scan]);

  // Parse the selected pipeline file.
  useEffect(() => {
    if (!sel) {
      setPipeline(null);
      return;
    }
    let stop = false;
    setParseError(null);
    api
      .ciPipeline(sel.repoPath, sel.file.provider, sel.file.file)
      .then((p) => !stop && setPipeline(p))
      .catch((e) => {
        if (stop) return;
        setPipeline(null);
        setParseError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      stop = true;
    };
  }, [sel]);

  // Live runs.
  useEffect(() => {
    let stop = false;
    const tick = async () => {
      try {
        const r = await api.ciRuns();
        if (!stop) setRuns([...r].sort((a, b) => b.startedMs - a.startedMs));
      } catch {
        /* ignore */
      }
    };
    void tick();
    const t = setInterval(tick, 1000);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, []);

  const run = useMemo(() => runs.find((r) => r.id === runId) ?? null, [runs, runId]);

  const start = async (o: CiRunOptions) => {
    if (!wsId) return;
    try {
      const id = await api.ciStart(wsId, o);
      lastOpts.current[id] = o;
      setRunId(id);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  };

  const rerun = async (r: CiRunView) => {
    const o: CiRunOptions = lastOpts.current[r.id] ?? {
      provider: r.provider,
      file: r.file,
      repoPath: r.repoPath,
      event: r.trigger.event,
      gitRef: r.trigger.tag || r.trigger.branch,
      source: r.trigger.working ? "working" : "committed",
      runner: "auto",
      onlyJobs: [],
      variables: [],
    };
    await start(o);
  };

  if (!ready) return null;
  if (!active) {
    return (
      <PageBody>
        <EmptyState icon="conversion_path" title="No workspace yet" action={<Btn variant="primary" icon="add" onClick={() => navigate("/workspaces/new")}>New workspace</Btn>}>
          The CI simulator looks for GitLab CI, GitHub Actions and Jenkins files in the repositories of a workspace.
        </EmptyState>
      </PageBody>
    );
  }

  const total = repos?.reduce((n, r) => n + r.pipelines.length, 0) ?? 0;

  return (
    <PageBody wide>
      <PageHeader
        title="CI Pipelines"
        subtitle="Run your GitLab CI, GitHub Actions and Jenkins pipelines locally: clean checkout, containers, services, artifacts and variables, the way the server does."
        actions={
          <>
            <Btn variant={showVars ? "primary" : "secondary"} icon="vpn_key" onClick={() => setShowVars(!showVars)}>
              Variables & secrets
            </Btn>
            <Btn icon="refresh" onClick={() => void scan()}>
              Rescan
            </Btn>
          </>
        }
      />

      {docker === false && (
        <Banner icon="warning" tone="warn">
          Docker is not running. Pipelines that need an image or services can't run in containers; with <b>Auto</b> they run on this machine instead, which is less like the server.
        </Banner>
      )}

      {showVars && (
        <div className="mb-space-md">
          <VarsCard />
        </div>
      )}

      {repos && total === 0 ? (
        <EmptyState icon="conversion_path" title="No pipeline files found" action={<Btn icon="edit" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Edit workspace</Btn>}>
          None of the workspace's repositories has a <code className="font-code-dense">.gitlab-ci.yml</code>, <code className="font-code-dense">.github/workflows/*.yml</code> or a{" "}
          <code className="font-code-dense">Jenkinsfile</code>.
        </EmptyState>
      ) : (
        <div className="grid gap-space-md" style={{ gridTemplateColumns: "300px minmax(0,1fr)" }}>
          <div className="space-y-space-md self-start">
            <Card title="Pipelines" icon="folder_open" bodyClass="p-0">
              {(repos ?? []).map((r) => (
                <div key={r.repoPath} className="border-b border-surface-container-highest last:border-0">
                  <div className="px-space-md pt-2 pb-1 text-[11px] font-label-sm text-outline truncate" title={r.repoPath}>
                    {repoName(r.repoPath)}
                  </div>
                  {r.pipelines.length === 0 && <div className="px-space-md pb-2 text-[12px] text-outline">No pipeline files</div>}
                  {r.pipelines.map((p) => {
                    const on = sel?.repoPath === r.repoPath && sel.file.file === p.file;
                    return (
                      <button
                        key={p.file}
                        type="button"
                        onClick={() => {
                          setSel({ repoPath: r.repoPath, file: p });
                          setRunId(null);
                        }}
                        className={`w-full text-left px-space-md py-1.5 flex items-center gap-2 ${on && !runId ? "bg-primary/10" : "hover:bg-surface-container-high/50"}`}
                      >
                        <ProviderBadge p={p.provider} />
                        <span className="font-code-dense text-code-dense text-on-surface truncate" title={p.file}>
                          {p.file.replace(".github/workflows/", "")}
                        </span>
                      </button>
                    );
                  })}
                </div>
              ))}
            </Card>

            <Card title="Runs" icon="history" bodyClass="p-0" actions={runs.length ? <span className="text-[11px] text-outline">{runs.length}</span> : undefined}>
              {runs.length === 0 ? (
                <div className="p-space-md text-[12px] text-outline">Nothing has run yet.</div>
              ) : (
                <ul className="max-h-[420px] overflow-auto">
                  {runs.map((r) => (
                    <li key={r.id}>
                      <button
                        type="button"
                        onClick={() => setRunId(r.id)}
                        className={`w-full text-left px-space-md py-2 border-b border-surface-container-highest last:border-0 flex items-start gap-2 ${runId === r.id ? "bg-primary/10" : "hover:bg-surface-container-high/50"}`}
                      >
                        <StatusIcon status={r.status} size={16} />
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center justify-between gap-2">
                            <span className="font-label-md text-label-md text-on-surface truncate">
                              {repoName(r.repoPath)} <span className="text-outline">#{r.number}</span>
                            </span>
                            <span className="text-[10px] text-outline shrink-0">{timeAgo(r.startedMs)}</span>
                          </div>
                          <div className="text-[11px] text-on-surface-variant truncate">
                            {r.provider} · {r.trigger.tag || r.trigger.branch || r.trigger.refName}@{r.trigger.shortSha} · {dur(r.startedMs, r.finishedMs)}
                          </div>
                        </div>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          </div>

          <div className="min-w-0">
            {run ? (
              <RunPanel run={run} onRerun={() => void rerun(run)} onDeleted={() => setRunId(null)} toast={toast} />
            ) : parseError ? (
              <Banner icon="error" tone="error">
                <b>{sel?.file.file}</b> could not be read: {parseError}
              </Banner>
            ) : pipeline && sel ? (
              <>
                {pipeline.warnings.length > 0 && (
                  <Banner icon="warning" tone="warn">
                    <div className="font-label-md mb-0.5">The simulator noticed {pipeline.warnings.length} thing(s) it can't reproduce exactly</div>
                    <ul className="list-disc pl-4 text-[12px] space-y-0.5">
                      {pipeline.warnings.map((w, i) => (
                        <li key={i}>{w}</li>
                      ))}
                    </ul>
                  </Banner>
                )}
                <RunForm sel={sel} pipeline={pipeline} onStart={start} docker={docker} />
              </>
            ) : (
              <div className="p-space-lg text-center text-on-surface-variant font-body-sm">{repos ? "Select a pipeline." : "Looking for pipelines…"}</div>
            )}
          </div>
        </div>
      )}
    </PageBody>
  );
}
