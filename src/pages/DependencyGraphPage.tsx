import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Btn, EmptyState, Icon, STATUS_UI, StatusPill, displayStatus, type DisplayStatus } from "../components/ui";
import type { Service } from "../lib/types";
import { useApp } from "../state/AppState";

const NODE_W = 190;
const NODE_H = 78;
const COL_GAP = 70;
const ROW_GAP = 26;
const PAD = 32;

function levelsOf(services: Service[]): { levels: Map<string, number>; cycle: boolean } {
  const names = new Set(services.map((s) => s.name));
  const levels = new Map<string, number>();
  let cycle = false;
  const visiting = new Set<string>();
  const depth = (n: string): number => {
    if (levels.has(n)) return levels.get(n)!;
    if (visiting.has(n)) {
      cycle = true;
      return 0;
    }
    visiting.add(n);
    const svc = services.find((s) => s.name === n)!;
    const d = Math.max(-1, ...svc.dependsOn.filter((x) => names.has(x) && x !== n).map(depth)) + 1;
    visiting.delete(n);
    levels.set(n, d);
    return d;
  };
  services.forEach((s) => depth(s.name));
  return { levels, cycle };
}

const EDGE_CLR: Record<DisplayStatus, string> = {
  running: "#4edea3",
  degraded: "#ffb95f",
  starting: "#ffb95f",
  stopping: "#ffb95f",
  failed: "#ffb4ab",
  stopped: "#424754",
  idle: "#424754",
};

export default function DependencyGraphPage() {
  const { active, runtime, up } = useApp();
  const navigate = useNavigate();
  const [zoom, setZoom] = useState(1);
  const [hover, setHover] = useState<string | null>(null);

  const services = active?.services ?? [];
  const statusOf = (n: string): DisplayStatus => displayStatus(runtime[n]);

  const layout = useMemo(() => {
    const { levels, cycle } = levelsOf(services);
    const cols: Service[][] = [];
    services.forEach((s) => {
      const l = levels.get(s.name) ?? 0;
      (cols[l] ??= []).push(s);
    });
    const pos = new Map<string, { x: number; y: number }>();
    cols.forEach((col, ci) =>
      col?.forEach((s, ri) => pos.set(s.name, { x: PAD + ci * (NODE_W + COL_GAP), y: PAD + ri * (NODE_H + ROW_GAP) })),
    );
    const width = PAD * 2 + cols.length * NODE_W + Math.max(0, cols.length - 1) * COL_GAP;
    const height = PAD * 2 + Math.max(1, ...cols.map((c) => c?.length ?? 0)) * (NODE_H + ROW_GAP);
    return { cols, pos, width, height, cycle };
  }, [services]);

  if (!active) {
    return (
      <div className="p-margin">
        <EmptyState icon="account_tree" title="No active workspace" action={<Btn variant="primary" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          The dependency graph is drawn from the “Starts after” links between services.
        </EmptyState>
      </div>
    );
  }
  if (services.length === 0) {
    return (
      <div className="p-margin">
        <EmptyState icon="account_tree" title="No services yet" action={<Btn variant="primary" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Add services</Btn>}>
          Add services and define what each one starts after.
        </EmptyState>
      </div>
    );
  }

  const edges = services.flatMap((s) =>
    s.dependsOn.filter((d) => layout.pos.has(d)).map((d) => ({ from: d, to: s.name })),
  );
  const related = (n: string | null) => {
    if (!n) return null;
    const set = new Set<string>([n]);
    let grew = true;
    while (grew) {
      grew = false;
      edges.forEach((e) => {
        if (set.has(e.to) && !set.has(e.from)) (set.add(e.from), (grew = true));
        if (set.has(e.from) && !set.has(e.to)) (set.add(e.to), (grew = true));
      });
    }
    return set;
  };
  const hl = related(hover);

  const failed = services.filter((s) => statusOf(s.name) === "failed");
  const blocked = (n: string) => {
    const svc = services.find((s) => s.name === n)!;
    return svc.dependsOn.some((d) => ["failed", "idle", "stopped"].includes(statusOf(d))) && !["running", "degraded", "starting"].includes(statusOf(n));
  };
  const conflict = up?.conflict && up.workspaceId === active.id ? up.conflict : null;

  return (
    <div className="flex flex-col" style={{ height: "calc(100vh - 3.5rem - 1.5rem)" }}>
      <div className="flex items-center justify-between px-margin py-space-sm border-b border-surface-container-highest bg-surface-container-low">
        <div className="flex items-center gap-3">
          <h1 className="font-headline-md text-headline-md text-on-surface">Dependency graph</h1>
          <span className="text-[11px] text-on-surface-variant font-code-dense">{services.length} services · {edges.length} links · {layout.cols.length} startup waves</span>
        </div>
        <div className="flex items-center gap-1">
          <Btn variant="ghost" icon="remove" className="!px-2" onClick={() => setZoom((z) => Math.max(0.5, +(z - 0.1).toFixed(1)))} />
          <span className="font-code-dense text-code-dense text-on-surface-variant w-10 text-center">{Math.round(zoom * 100)}%</span>
          <Btn variant="ghost" icon="add" className="!px-2" onClick={() => setZoom((z) => Math.min(1.6, +(z + 0.1).toFixed(1)))} />
          <Btn variant="ghost" className="!px-2" onClick={() => setZoom(1)}>Reset</Btn>
          <Btn icon="edit" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Edit links</Btn>
        </div>
      </div>

      {(failed.length > 0 || layout.cycle) && (
        <div className="flex items-center gap-2 px-margin py-space-sm bg-error-container/30 border-b border-error/30 text-body-sm">
          <Icon name="error" className="text-error text-[18px]" />
          {layout.cycle ? (
            <span className="text-on-surface">Circular dependency detected: Up will refuse to run until it is resolved.</span>
          ) : (
            <span className="text-on-surface">
              <b className="text-error">{failed.map((f) => f.name).join(", ")}</b> failed
              {conflict && <> · port :{conflict.port} held by {conflict.process} (PID {conflict.pid})</>}
              {" "}: services that depend on {failed.length > 1 ? "them" : "it"} cannot start.
            </span>
          )}
          <Btn variant="ghost" className="ml-auto !py-0.5" onClick={() => navigate("/up")}>View pipeline</Btn>
        </div>
      )}

      <div className="flex-1 min-h-0 flex">
        <div className="flex-1 overflow-auto bg-surface-container-lowest" style={{ backgroundImage: "radial-gradient(#31353c 1px, transparent 1px)", backgroundSize: "20px 20px" }}>
          <div style={{ width: layout.width * zoom, height: layout.height * zoom }}>
            <div className="relative origin-top-left" style={{ width: layout.width, height: layout.height, transform: `scale(${zoom})` }}>
              <svg className="absolute inset-0 pointer-events-none" width={layout.width} height={layout.height}>
                <defs>
                  {Object.entries(EDGE_CLR).map(([k, c]) => (
                    <marker key={k} id={`arrow-${k}`} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
                      <path d="M0,0 L10,5 L0,10 z" fill={c} />
                    </marker>
                  ))}
                </defs>
                {edges.map((e) => {
                  const a = layout.pos.get(e.from)!;
                  const b = layout.pos.get(e.to)!;
                  const x1 = a.x + NODE_W;
                  const y1 = a.y + NODE_H / 2;
                  const x2 = b.x;
                  const y2 = b.y + NODE_H / 2;
                  const mx = (x1 + x2) / 2;
                  const st = statusOf(e.from);
                  const dim = hl && !(hl.has(e.from) && hl.has(e.to));
                  return (
                    <path
                      key={`${e.from}>${e.to}`}
                      d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2 - 2},${y2}`}
                      fill="none"
                      stroke={EDGE_CLR[st]}
                      strokeWidth={hl && !dim ? 2.5 : 1.5}
                      strokeDasharray={st === "running" || st === "degraded" ? undefined : "5 4"}
                      opacity={dim ? 0.15 : 0.9}
                      markerEnd={`url(#arrow-${st})`}
                    />
                  );
                })}
              </svg>

              {layout.cols.map((_, ci) => (
                <div key={ci} className="absolute text-[10px] font-label-sm uppercase tracking-wider text-outline" style={{ left: PAD + ci * (NODE_W + COL_GAP), top: 12 }}>
                  Wave {ci + 1}
                </div>
              ))}

              {services.map((s) => {
                const p = layout.pos.get(s.name)!;
                const st = statusOf(s.name);
                const rt = runtime[s.name];
                const dim = hl && !hl.has(s.name);
                const isBlocked = blocked(s.name);
                return (
                  <button
                    key={s.name}
                    type="button"
                    onMouseEnter={() => setHover(s.name)}
                    onMouseLeave={() => setHover(null)}
                    onClick={() => navigate(`/services/${encodeURIComponent(s.name)}`)}
                    className={`absolute text-left bg-surface-container border rounded-lg px-3 py-2 shadow-md transition-opacity ${st === "failed" ? "border-error" : st === "running" ? "border-secondary/60" : st === "starting" || st === "degraded" ? "border-tertiary/60" : "border-outline-variant"} ${dim ? "opacity-30" : ""}`}
                    style={{ left: p.x, top: p.y, width: NODE_W, height: NODE_H, marginTop: 24 }}
                  >
                    <div className="flex items-center justify-between gap-1">
                      <span className="flex items-center gap-1.5 min-w-0">
                        <span className={`w-2 h-2 rounded-full shrink-0 ${STATUS_UI[st].dot}`} />
                        <span className="font-headline-sm text-headline-sm text-on-surface truncate">{s.name}</span>
                      </span>
                      {s.port && <span className="font-code-dense text-code-dense px-1 rounded bg-surface-container-highest text-on-surface-variant">:{s.port}</span>}
                    </div>
                    <div className="flex items-center gap-1 mt-1 text-[10px] text-on-surface-variant">
                      <Icon name={s.kind === "infra" ? "database" : "terminal"} className="text-[12px]" />
                      <span className="truncate">{s.kind === "infra" ? s.image : s.stack || "app"}</span>
                    </div>
                    <div className="flex items-center justify-between mt-1 font-code-dense text-code-dense">
                      <span className={STATUS_UI[st].text}>{STATUS_UI[st].label}</span>
                      <span className="text-outline">{rt?.memMb != null ? `${rt.memMb.toFixed(0)} MB` : isBlocked ? "waiting on deps" : ""}</span>
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
        </div>

        <aside className="w-80 shrink-0 border-l border-surface-container-highest bg-surface-container-low overflow-y-auto">
          <div className="p-space-md border-b border-surface-container-highest">
            <h2 className="font-headline-sm text-headline-sm text-on-surface flex items-center gap-1.5"><Icon name="rocket_launch" className="text-[18px]" /> Startup order</h2>
            <p className="text-[11px] text-on-surface-variant mt-0.5">Each wave starts only after the previous one is healthy.</p>
          </div>
          <div className="p-space-md space-y-space-sm">
            {layout.cols.map((col, ci) => (
              <div key={ci} className="rounded-lg border border-surface-container-highest bg-surface-container p-space-sm">
                <div className="font-label-sm text-label-sm text-on-surface-variant mb-1.5">Wave {ci + 1}</div>
                <div className="space-y-1">
                  {col?.map((s, i) => (
                    <button key={s.name} type="button" onClick={() => navigate(`/services/${encodeURIComponent(s.name)}`)} className="w-full flex items-center justify-between px-2 py-1 rounded bg-surface-container-lowest hover:bg-surface-container-high">
                      <span className="font-code-dense text-code-dense text-on-surface">{ci + 1}.{i + 1} {s.name}{s.port ? `:${s.port}` : ""}</span>
                      <StatusPill status={statusOf(s.name)} />
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </aside>
      </div>
    </div>
  );
}
