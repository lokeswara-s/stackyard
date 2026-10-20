import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Btn, Icon, STEP_ICON } from "../components/ui";
import { useLogs } from "../hooks/useLogs";
import { api } from "../lib/tauri";
import type { Stage } from "../lib/types";
import { useApp } from "../state/AppState";
import DashboardPage from "./DashboardPage";

function fmt(ms: number) {
  const s = Math.max(0, ms) / 1000;
  const m = Math.floor(s / 60);
  return m > 0 ? `${m}m ${(s % 60).toFixed(0)}s` : `${s.toFixed(1)}s`;
}

const LEVEL_CLS: Record<string, string> = { error: "text-error", warn: "text-tertiary", debug: "text-outline", info: "text-on-surface-variant" };

function StageCard({ stage, index, now }: { stage: Stage; index: number; now: number }) {
  const ic = STEP_ICON[stage.status === "pending" ? "pending" : stage.status];
  const dur = stage.startedMs ? fmt((stage.finishedMs ?? now) - stage.startedMs) : null;
  const tone =
    stage.status === "failed" ? "border-error/50 bg-error-container/10" : stage.status === "running" ? "border-primary/50" : "border-surface-container-highest";
  return (
    <div className={`rounded-lg border ${tone} bg-surface-container p-space-md ${stage.status === "pending" ? "opacity-60" : ""}`}>
      <div className="flex items-center gap-2">
        <Icon name={ic.icon} className={`text-[20px] ${ic.cls}`} />
        <div className="flex-1 min-w-0">
          <div className="font-headline-sm text-headline-sm text-on-surface">
            {index + 1}. {stage.title}
          </div>
          {stage.detail && <div className="text-[11px] text-on-surface-variant select-text">{stage.detail}</div>}
        </div>
        {dur && <span className="font-code-dense text-code-dense text-outline">{dur}</span>}
        <span className="text-[10px] uppercase tracking-wider text-outline">{stage.status}</span>
      </div>
      {stage.items.length > 0 && stage.status !== "skipped" && (
        <div className="mt-2 space-y-1">
          {stage.items.map((it) => {
            const i = STEP_ICON[it.status];
            return (
              <div key={it.name} className="flex items-start gap-2 pl-7 text-[11px] font-code-dense">
                <Icon name={i.icon} className={`text-[14px] mt-px ${i.cls}`} />
                <span className="text-on-surface w-40 shrink-0 truncate">{it.name}</span>
                <span className={`select-text ${it.status === "failed" ? "text-error" : it.status === "warn" ? "text-tertiary" : "text-on-surface-variant"}`}>{it.detail}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export default function UpPipelinePage() {
  const { up, active, startUp, cancelUp, stopAll, toast, refreshRuntime } = useApp();
  const navigate = useNavigate();
  const [now, setNow] = useState(Date.now());
  const logRef = useRef<HTMLDivElement>(null);
  const { lines } = useLogs(active?.id);

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(id);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && navigate("/");
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate]);
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines.length]);

  const run = up && active && up.workspaceId === active.id ? up : null;
  const running = run?.status === "running";
  const elapsed = run ? (run.finishedMs ?? now) - run.startedMs : 0;
  const doneStages = run ? run.stages.filter((s) => ["passed", "skipped"].includes(s.status)).length : 0;
  const pct = run ? Math.round((doneStages / run.stages.length) * 100) : 0;

  const killAndRetry = async () => {
    if (!run?.conflict) return;
    try {
      if (run.conflict.container) {
        // Never kill Docker's engine process: stop only the container that holds the port.
        await api.dockerAction("stop", run.conflict.container);
        toast(`Stopped container ${run.conflict.container}`, "success");
      } else {
        await api.killPid(run.conflict.pid);
        toast(`Killed ${run.conflict.process} (PID ${run.conflict.pid})`, "success");
      }
      await startUp();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  };

  const tone = !run ? "text-outline" : run.status === "success" ? "text-secondary" : run.status === "failed" ? "text-error" : run.status === "cancelled" ? "text-tertiary" : "text-primary";
  const headline = !run
    ? "No run yet"
    : run.status === "running"
      ? "Bringing the workspace up…"
      : run.status === "success"
        ? "Workspace is up"
        : run.status === "cancelled"
          ? "Run cancelled"
          : "Up failed";

  return (
    <>
      <div className="pointer-events-none select-none" aria-hidden="true">
        <DashboardPage />
      </div>
      <div className="fixed left-64 right-0 top-14 bottom-6 z-30 bg-surface/80 backdrop-blur-sm flex items-start justify-center overflow-y-auto p-space-xl">
        <div className="w-full max-w-3xl bg-surface-container-low border border-outline-variant rounded-xl shadow-2xl overflow-hidden">
          <div className="p-space-lg border-b border-surface-container-highest">
            <div className="flex items-start justify-between gap-4">
              <div>
                <div className="flex items-center gap-2">
                  <Icon name={running ? "progress_activity" : run?.status === "success" ? "check_circle" : run?.status === "failed" ? "error" : "sync"} className={`text-[22px] ${tone} ${running ? "animate-spin" : ""}`} />
                  <h1 className="font-headline-lg text-headline-lg text-on-surface">{headline}</h1>
                </div>
                <p className="text-body-sm text-on-surface-variant mt-1">
                  {active?.name} · profile <span className="font-code-dense text-primary">{run?.profile ?? active?.profile}</span>
                  {run && (
                    <>
                      {" "}· elapsed <span className="font-code-dense text-on-surface">{fmt(elapsed)}</span>
                    </>
                  )}
                </p>
              </div>
              <div className="flex items-center gap-2">
                {running ? (
                  <Btn variant="danger" icon="cancel" onClick={() => void cancelUp()}>
                    Cancel run
                  </Btn>
                ) : (
                  <Btn variant="primary" icon="replay" onClick={() => void startUp()} disabled={!active}>
                    {run ? "Run again" : "Start Up"}
                  </Btn>
                )}
                <Btn variant="ghost" icon="close" onClick={() => navigate("/")}>
                  Close
                </Btn>
              </div>
            </div>
            {run && (
              <div className="mt-3 h-1 bg-surface-container-highest rounded-full overflow-hidden">
                <div className={`h-full transition-all ${run.status === "failed" ? "bg-error" : "bg-secondary"}`} style={{ width: `${run.status === "success" ? 100 : pct}%` }} />
              </div>
            )}
          </div>

          <div className="p-space-lg space-y-space-sm">
            {run?.error && (
              <div className="rounded-lg border border-error/40 bg-error-container/20 p-space-md text-body-sm text-on-surface select-text">
                <b className="text-error">Stopped:</b> {run.error}
                {run.conflict && (
                  <div className="mt-2 flex items-center gap-2">
                    <Btn variant="danger" icon="bolt" onClick={() => void killAndRetry()}>
                      {run.conflict.container
                        ? `Stop container ${run.conflict.container} & retry`
                        : `Kill ${run.conflict.process} (PID ${run.conflict.pid}) & retry`}
                    </Btn>
                    <span className="text-[11px] text-on-surface-variant">frees port :{run.conflict.port} for {run.conflict.service}</span>
                  </div>
                )}
              </div>
            )}
            {run ? (
              run.stages.map((s, i) => <StageCard key={s.id} stage={s} index={i} now={now} />)
            ) : (
              <div className="text-on-surface-variant text-body-sm py-6 text-center">Press “Start Up” to run the pipeline.</div>
            )}
            {run && run.status !== "running" && (
              <div className="flex items-center gap-2 pt-2">
                {run.status === "success" && (
                  <Btn variant="success" icon="dashboard" onClick={() => navigate("/")}>
                    Open dashboard
                  </Btn>
                )}
                {run.status !== "success" && (
                  <Btn
                    variant="danger"
                    icon="stop_circle"
                    onClick={async () => {
                      await stopAll();
                      await refreshRuntime();
                    }}
                  >
                    Stop what started
                  </Btn>
                )}
                <Btn icon="segment" onClick={() => navigate("/logs")}>
                  Open logs
                </Btn>
              </div>
            )}
          </div>

          <div className="border-t border-surface-container-highest bg-surface-container-lowest">
            <div className="flex items-center gap-2 px-space-md py-space-xs text-[11px] font-label-sm text-on-surface-variant border-b border-surface-container-highest">
              <Icon name="terminal" className="text-[14px]" /> Live log tail
            </div>
            <div ref={logRef} className="h-48 overflow-y-auto font-code-dense text-code-dense p-space-sm select-text" style={{ scrollbarWidth: "thin" }}>
              {lines.length === 0 && <div className="text-outline">No output yet.</div>}
              {lines.slice(-200).map((l) => (
                <div key={l.seq} className={`whitespace-pre-wrap break-all ${LEVEL_CLS[l.level]}`}>
                  <span className="text-secondary">[{l.service}]</span> {l.text}
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
