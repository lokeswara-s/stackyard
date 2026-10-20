import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";

import { Btn, EmptyState, Icon, PageBody, PageHeader, StatusPill, STATUS_UI, displayStatus, formatUptime, type DisplayStatus } from "../components/ui";
import { openExternal } from "../lib/tauri";
import type { Service, ServiceRuntime } from "../lib/types";
import { useApp } from "../state/AppState";

type Filter = "all" | "running" | "degraded" | "starting" | "failed" | "stopped";
const FILTERS: { id: Filter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "running", label: "Healthy" },
  { id: "degraded", label: "Degraded" },
  { id: "starting", label: "Starting" },
  { id: "failed", label: "Failed" },
  { id: "stopped", label: "Stopped" },
];

function matches(f: Filter, s: DisplayStatus): boolean {
  if (f === "all") return true;
  if (f === "stopped") return s === "stopped" || s === "idle";
  if (f === "starting") return s === "starting" || s === "stopping";
  return s === f;
}

function ServiceCard({ svc, rt }: { svc: Service; rt: ServiceRuntime }) {
  const { startService, stopService, restartService } = useApp();
  const navigate = useNavigate();
  const alive = rt.status === "running" || rt.status === "starting" || rt.status === "stopping";
  const ds = displayStatus(rt);
  const border =
    ds === "failed"
      ? "border-error/50"
      : ds === "degraded"
        ? "border-tertiary/60"
        : ds === "running"
          ? "border-surface-container-highest hover:border-secondary/50"
          : "border-surface-container-highest hover:border-outline";

  return (
    <div
      onClick={() => navigate(`/services/${encodeURIComponent(svc.name)}`)}
      className={`group cursor-pointer bg-surface-container border ${border} rounded-lg p-space-md flex flex-col gap-space-sm transition-colors`}
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5">
            <span className={`w-2 h-2 rounded-full shrink-0 ${STATUS_UI[ds].dot}`} />
            <h3 className="font-headline-sm text-headline-sm text-on-surface truncate">{svc.name}</h3>
          </div>
          <div className="flex items-center gap-1.5 mt-1 text-[10px] font-label-sm text-on-surface-variant">
            <Icon name={svc.kind === "infra" ? "database" : "terminal"} className="text-[13px]" />
            <span>{svc.kind === "infra" ? svc.image : svc.stack || "app"}</span>
            {rt.chaos === "faulting" && (
              <Link
                to="/fault-injection"
                onClick={(e) => e.stopPropagation()}
                title="Faults are being injected into this service"
                className="inline-flex items-center gap-0.5 px-1 rounded bg-tertiary/20 text-tertiary hover:bg-tertiary/30"
              >
                <Icon name="bolt" className="text-[12px]" /> faults on
              </Link>
            )}
            {(rt.chaos === "proxy" || rt.chaos === "armed") && (
              <span title="Routed through the fault proxy, no faults set" className="inline-flex items-center gap-0.5 px-1 rounded bg-surface-container-highest text-outline">
                <Icon name="bolt" className="text-[12px]" /> {rt.chaos === "armed" ? "proxy armed" : "proxied"}
              </span>
            )}
          </div>
        </div>
        <StatusPill status={ds} />
      </div>

      <div className="grid grid-cols-3 gap-2 font-code-dense text-code-dense">
        <div>
          <div className="text-outline">PORT</div>
          {(() => {
            // Prefer the port the service is really listening on over the configured one.
            const shown = rt.livePort ?? svc.port ?? null;
            if (shown == null) {
              return <span className="text-outline">{alive && !rt.portOpen ? "detecting…" : alive ? "none" : "auto"}</span>;
            }
            const cls = rt.portMismatch ? "text-tertiary" : rt.portOpen ? "text-primary" : "text-on-surface-variant";
            const tip = rt.portMismatch
              ? `Configured :${svc.port} but listening on :${shown}`
              : svc.port == null
                ? `Detected :${shown}`
                : `:${shown}`;
            if (svc.kind !== "app") return <span className={rt.portOpen ? "text-secondary" : "text-on-surface-variant"}>:{shown}</span>;
            return (
              <a
                href={`http://localhost:${shown}`}
                title={tip}
                target="_blank"
                rel="noreferrer"
                onClick={(e) => {
                  e.stopPropagation();
                  e.preventDefault();
                  void openExternal(`http://localhost:${shown}`);
                }}
                className={`${cls} hover:underline inline-flex items-center gap-0.5`}
              >
                {rt.portMismatch && <Icon name="warning" className="text-[12px]" />}:{shown}
                {svc.port == null && rt.portOpen && <span className="text-outline no-underline ml-1">auto</span>}
              </a>
            );
          })()}
        </div>
        <div>
          <div className="text-outline">UPTIME</div>
          <span className="text-on-surface">{alive ? formatUptime(rt.startedMs) : "–"}</span>
        </div>
        <div>
          <div className="text-outline">MEMORY</div>
          <span className="text-on-surface">{rt.memMb != null ? `${rt.memMb.toFixed(0)} MB` : "–"}</span>
        </div>
      </div>

      {ds === "degraded" && (
        <Link
          to={`/logs?service=${encodeURIComponent(svc.name)}&level=error`}
          onClick={(e) => e.stopPropagation()}
          className="block rounded border border-tertiary/40 bg-tertiary/10 px-2 py-1.5 text-[11px] leading-snug hover:bg-tertiary/15"
        >
          <div className="flex items-center gap-1 text-tertiary font-label-sm">
            <Icon name="warning" className="text-[14px]" />
            {rt.errorCount} error{rt.errorCount === 1 ? "" : "s"} in the last minute
          </div>
          {rt.healthHint && <div className="text-on-surface mt-0.5">{rt.healthHint}</div>}
          {rt.lastError && <div className="text-on-surface-variant font-code-dense truncate mt-0.5">{rt.lastError}</div>}
        </Link>
      )}

      {svc.dependsOn.length > 0 && (
        <div className="flex flex-wrap items-center gap-1 text-[10px] text-outline">
          <Icon name="subdirectory_arrow_right" className="text-[13px]" />
          {svc.dependsOn.map((d) => (
            <span key={d} className="px-1.5 py-0.5 rounded bg-surface-container-highest text-on-surface-variant font-code-dense">
              {d}
            </span>
          ))}
        </div>
      )}

      <div className="flex items-center gap-1.5 mt-auto pt-space-xs border-t border-surface-container-highest" onClick={(e) => e.stopPropagation()}>
        {alive ? (
          <>
            <Btn icon="stop" variant="danger" className="!px-2 !py-1" onClick={() => void stopService(svc.name)} disabled={rt.status === "stopping"}>
              Stop
            </Btn>
            <Btn icon="restart_alt" className="!px-2 !py-1" onClick={() => void restartService(svc.name)}>
              Restart
            </Btn>
          </>
        ) : (
          <Btn icon="play_arrow" variant="success" className="!px-2 !py-1" onClick={() => void startService(svc.name)}>
            Start
          </Btn>
        )}
        <Link to={`/logs?service=${encodeURIComponent(svc.name)}`} className="ml-auto text-[11px] text-on-surface-variant hover:text-primary flex items-center gap-1">
          <Icon name="segment" className="text-[14px]" /> Logs
        </Link>
      </div>
    </div>
  );
}

export default function DashboardPage() {
  const { active, runtime, up, startUp, stopAll, ready } = useApp();
  const navigate = useNavigate();
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");

  const services = active?.services ?? [];
  const rtOf = (name: string): ServiceRuntime => runtime[name] ?? { name, status: "idle", portOpen: false };

  const counts = useMemo(() => {
    const c: Record<Filter, number> = { all: services.length, running: 0, degraded: 0, starting: 0, failed: 0, stopped: 0 };
    for (const s of services) {
      const st = displayStatus(runtime[s.name]);
      (["running", "degraded", "starting", "failed", "stopped"] as const).forEach((f) => {
        if (matches(f, st)) c[f]++;
      });
    }
    return c;
  }, [services, runtime]);

  const visible = services.filter((s) => {
    const q = query.trim().toLowerCase();
    const hit = !q || s.name.toLowerCase().includes(q) || s.stack.toLowerCase().includes(q) || String(s.port ?? "").includes(q);
    return hit && matches(filter, displayStatus(rtOf(s.name)));
  });

  if (!ready) return null;

  if (!active) {
    return (
      <PageBody>
        <EmptyState
          icon="rocket_launch"
          title="Create your first workspace"
          action={<Btn variant="primary" icon="add" onClick={() => navigate("/workspaces/new")}>New workspace</Btn>}
        >
          A workspace groups the repositories, infra containers, ports and env config that make up one product. Then one click on <b>Up</b> syncs,
          configures and starts everything in dependency order.
        </EmptyState>
      </PageBody>
    );
  }

  const anyAlive = services.some((s) => ["running", "starting"].includes(rtOf(s.name).status));
  const upRunning = up?.status === "running" && up.workspaceId === active.id;

  return (
    <PageBody>
      <PageHeader
        title={active.name}
        subtitle={
          <>
            {active.description || "Local workspace"} · profile <span className="font-code-dense text-primary">{active.profile}</span>
          </>
        }
        actions={
          <>
            <Btn icon="edit" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>
              Edit
            </Btn>
            <Btn icon="stop_circle" variant="danger" disabled={!anyAlive} onClick={() => void stopAll()}>
              Stop all
            </Btn>
            <Btn
              icon={upRunning ? "progress_activity" : "play_arrow"}
              variant="success"
              disabled={services.length === 0}
              onClick={async () => {
                if (upRunning || (await startUp())) navigate("/up");
              }}
            >
              {upRunning ? "Up running…" : "Up"}
            </Btn>
          </>
        }
      />

      {services.length === 0 ? (
        <EmptyState
          icon="deployed_code"
          title="No services in this workspace yet"
          action={<Btn variant="primary" icon="add" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Add services</Btn>}
        >
          Add repositories (we auto-detect the start command) and infra such as Postgres or Redis.
        </EmptyState>
      ) : (
        <>
          <div className="flex items-center justify-between gap-space-md mb-space-md">
            <div className="flex items-center bg-surface-container-lowest p-0.5 rounded-lg border border-surface-container-highest">
              {FILTERS.map((f) => (
                <button
                  key={f.id}
                  type="button"
                  onClick={() => setFilter(f.id)}
                  className={`px-3 py-1 rounded font-label-sm text-label-sm ${filter === f.id ? "bg-surface-container-high text-primary" : "text-on-surface-variant hover:text-on-surface"}`}
                >
                  {f.label} <span className="text-outline ml-0.5">{counts[f.id]}</span>
                </button>
              ))}
            </div>
            <div className="relative w-64">
              <Icon name="search" className="absolute left-2 top-1/2 -translate-y-1/2 text-[16px] text-outline" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Filter services…"
                className="w-full h-8 pl-7 pr-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text"
              />
            </div>
          </div>

          {visible.length === 0 ? (
            <div className="text-center py-12 text-on-surface-variant font-body-sm">No services match this filter.</div>
          ) : (
            <div className="grid gap-space-md" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))" }}>
              {visible.map((s) => (
                <ServiceCard key={s.name} svc={s} rt={rtOf(s.name)} />
              ))}
            </div>
          )}
        </>
      )}
    </PageBody>
  );
}
