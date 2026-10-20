import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";

import { Banner, Btn, Card, EmptyState, Icon, PageBody, StatusPill, displayStatus, formatUptime } from "../components/ui";
import { useLogs } from "../hooks/useLogs";
import { api, openExternal, revealPath } from "../lib/tauri";
import type { GitStatus, ServiceRuntime } from "../lib/types";
import { useApp } from "../state/AppState";

const LEVEL_CLS: Record<string, string> = { error: "text-error", warn: "text-tertiary", debug: "text-outline", info: "text-on-surface" };

function Metric({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="bg-surface-container border border-surface-container-highest rounded-lg px-space-md py-space-sm">
      <div className="font-code-dense text-code-dense text-outline uppercase">{label}</div>
      <div className="font-headline-md text-headline-md text-on-surface">{value}</div>
      {sub && <div className="font-code-dense text-code-dense text-on-surface-variant">{sub}</div>}
    </div>
  );
}

export default function ServiceDetailPage() {
  const params = useParams();
  const name = decodeURIComponent(params.name ?? "");
  const { active, runtime, startService, stopService, restartService } = useApp();
  const navigate = useNavigate();
  const svc = active?.services.find((s) => s.name === name);
  const rt: ServiceRuntime = runtime[name] ?? { name, status: "idle", portOpen: false };
  const { lines, clear } = useLogs(active?.id);
  const mine = useMemo(() => lines.filter((l) => l.service === name), [lines, name]);
  const logRef = useRef<HTMLDivElement>(null);
  const [git, setGit] = useState<GitStatus | null>(null);
  const [reveal, setReveal] = useState<Record<string, boolean>>({});

  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [mine.length]);

  useEffect(() => {
    setGit(null);
    if (svc?.kind === "app" && svc.repoPath) {
      api.gitStatus([svc.repoPath]).then((r) => setGit(r[0] ?? null)).catch(() => {});
    }
  }, [svc?.repoPath, svc?.kind]);

  if (!active || !svc) {
    return (
      <PageBody>
        <EmptyState icon="search_off" title="Service not found" action={<Btn onClick={() => navigate("/")}>Back to dashboard</Btn>}>
          “{name}” is not part of the active workspace.
        </EmptyState>
      </PageBody>
    );
  }

  const alive = ["running", "starting", "stopping"].includes(rt.status);
  const shownPort = rt.livePort ?? svc.port ?? null;
  const dependents = active.services.filter((s) => s.dependsOn.includes(svc.name));
  const envVars = [...active.env, ...svc.env].filter((v) => v.key && (!v.profile || v.profile === active.profile));
  const merged = new Map<string, (typeof envVars)[number]>();
  envVars.forEach((v) => merged.set(v.key, v));
  if (svc.port && !merged.has("PORT")) merged.set("PORT", { key: "PORT", value: String(svc.port), secret: false });

  const statusOf = (n: string) => displayStatus(runtime[n]);

  return (
    <PageBody>
      <div className="text-[11px] font-code-dense text-outline mb-2">
        <Link to="/" className="hover:text-primary">{active.name}</Link> / <Link to="/" className="hover:text-primary">Services</Link> / <span className="text-on-surface-variant">{svc.name}</span>
      </div>

      <div className="flex items-start justify-between gap-space-lg mb-space-lg">
        <div>
          <div className="flex items-center gap-3">
            <h1 className="font-headline-lg text-headline-lg text-on-surface">{svc.name}</h1>
            <StatusPill status={displayStatus(rt)} />
          </div>
          <div className="flex items-center gap-2 mt-1 text-body-sm text-on-surface-variant">
            <Icon name={svc.kind === "infra" ? "database" : "terminal"} className="text-[16px]" />
            {svc.kind === "infra" ? `Docker · ${svc.image}` : svc.stack || "Application"}
            {git?.isRepo && (
              <span className="font-code-dense text-code-dense px-1.5 py-0.5 rounded bg-surface-container-highest">
                ⑂ {git.branch}
                {git.changes.length + git.untracked.length > 0 ? ` · ${git.changes.length + git.untracked.length} changes` : ""}
              </span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          {shownPort != null && svc.kind === "app" && (
            <Btn icon="open_in_new" disabled={!rt.portOpen} onClick={() => void openExternal(`http://localhost:${shownPort}`)}>
              Open :{shownPort}
            </Btn>
          )}
          <Btn icon="edit" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Edit</Btn>
          {alive ? (
            <>
              <Btn icon="restart_alt" onClick={() => void restartService(svc.name)}>Restart</Btn>
              <Btn icon="stop" variant="danger" onClick={() => void stopService(svc.name)} disabled={rt.status === "stopping"}>Stop</Btn>
            </>
          ) : (
            <Btn icon="play_arrow" variant="success" onClick={() => void startService(svc.name)}>Start</Btn>
          )}
        </div>
      </div>

      {displayStatus(rt) === "degraded" && (
        <Banner icon="warning" tone="warn">
          <b>
            Running, but {rt.errorCount} error{rt.errorCount === 1 ? "" : "s"} in the last minute.
          </b>{" "}
          {rt.healthHint}
          {rt.lastError && <div className="font-code-dense text-code-dense text-on-surface-variant mt-1 break-all">{rt.lastError}</div>}
          <div className="mt-1 text-[11px] text-on-surface-variant">Goes back to Healthy once a minute passes with no new errors.</div>
        </Banner>
      )}

      <div className="grid grid-cols-5 gap-space-md mb-space-md">
        <Metric label="Uptime" value={alive ? formatUptime(rt.startedMs) : "–"} sub={rt.startedMs ? `since ${new Date(rt.startedMs).toLocaleTimeString()}` : "not running"} />
        <Metric label="Process" value={rt.pid ? `PID ${rt.pid}` : "–"} sub={rt.exitCode != null && !alive ? `exit code ${rt.exitCode}` : undefined} />
        <Metric label="Memory" value={rt.memMb != null ? `${rt.memMb.toFixed(0)} MB` : "–"} sub="process tree" />
        <Metric label="CPU" value={rt.cpu != null ? `${rt.cpu.toFixed(1)}%` : "–"} sub="process tree" />
        <Metric
          label="Port"
          value={shownPort != null ? `:${shownPort}` : "–"}
          sub={
            rt.portMismatch
              ? `configured :${svc.port}, listening here`
              : shownPort == null
                ? alive
                  ? "detecting…"
                  : "auto-detected when running"
                : svc.port == null
                  ? "auto-detected"
                  : rt.portOpen
                    ? "accepting connections"
                    : "closed"
          }
        />
      </div>

      <div className="grid gap-space-md" style={{ gridTemplateColumns: "minmax(0,1fr) 360px" }}>
        <div className="space-y-space-md min-w-0">
          <Card
            title="Logs"
            icon="segment"
            bodyClass="p-0"
            actions={
              <div className="flex items-center gap-2">
                <Link to={`/logs?service=${encodeURIComponent(svc.name)}`} className="text-[11px] text-primary hover:underline">Open full viewer</Link>
                <Btn variant="ghost" icon="delete_sweep" className="!px-2 !py-0.5" onClick={() => void clear()}>Clear</Btn>
              </div>
            }
          >
            <div ref={logRef} className="h-80 overflow-y-auto bg-surface-container-lowest font-code-dense text-code-dense p-space-sm select-text rounded-b-lg">
              {mine.length === 0 && <div className="text-outline">No output captured yet. Start the service to see its logs.</div>}
              {mine.slice(-400).map((l) => (
                <div key={l.seq} className={`whitespace-pre-wrap break-all ${l.stream === "system" ? "text-primary" : LEVEL_CLS[l.level]}`}>
                  <span className="text-outline mr-2">{new Date(l.ts).toLocaleTimeString()}</span>
                  {l.text}
                </div>
              ))}
            </div>
          </Card>

          <Card title="Environment" icon="vpn_key" actions={<Link to="/environment" className="text-[11px] text-primary hover:underline">Edit variables</Link>} bodyClass="p-0">
            {merged.size === 0 ? (
              <div className="p-space-md text-body-sm text-on-surface-variant">No variables for profile “{active.profile}”.</div>
            ) : (
              <table className="w-full font-code-dense text-code-dense">
                <tbody>
                  {[...merged.values()].map((v) => (
                    <tr key={v.key} className="border-b border-surface-container-highest last:border-0">
                      <td className="px-space-md py-1.5 text-primary w-1/3">{v.key}</td>
                      <td className="px-space-md py-1.5 text-on-surface select-text break-all">
                        {v.secret && !reveal[v.key] ? "••••••••" : v.value}
                        {v.secret && (
                          <button type="button" className="ml-2 text-outline hover:text-on-surface" onClick={() => setReveal((r) => ({ ...r, [v.key]: !r[v.key] }))}>
                            <Icon name={reveal[v.key] ? "visibility_off" : "visibility"} className="text-[14px] align-middle" />
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Card>
        </div>

        <div className="space-y-space-md">
          <Card title="Runtime" icon="terminal">
            <dl className="space-y-space-sm text-body-sm">
              {svc.kind === "app" ? (
                <>
                  <div>
                    <dt className="font-label-sm text-label-sm text-outline">START COMMAND</dt>
                    <dd className="font-code-dense text-code-dense text-on-surface bg-surface-container-lowest rounded p-1.5 mt-0.5 break-all select-text">{svc.command || "–"}</dd>
                  </div>
                  <div>
                    <dt className="font-label-sm text-label-sm text-outline">WORKING DIRECTORY</dt>
                    <dd className="font-code-dense text-code-dense text-on-surface break-all select-text flex items-start gap-1">
                      <span className="flex-1">{svc.repoPath}</span>
                      <button type="button" title="Show in file explorer" onClick={() => void revealPath(svc.repoPath)} className="text-outline hover:text-primary">
                        <Icon name="folder_open" className="text-[16px]" />
                      </button>
                    </dd>
                  </div>
                </>
              ) : (
                <>
                  <div>
                    <dt className="font-label-sm text-label-sm text-outline">IMAGE</dt>
                    <dd className="font-code-dense text-code-dense text-on-surface select-text">{svc.image}</dd>
                  </div>
                  <div>
                    <dt className="font-label-sm text-label-sm text-outline">PORT MAPPING</dt>
                    <dd className="font-code-dense text-code-dense text-on-surface">{svc.port ? `${svc.port} → ${svc.containerPort ?? svc.port}` : "none"}</dd>
                  </div>
                  {svc.dataPath && (
                    <div>
                      <dt className="font-label-sm text-label-sm text-outline">PERSISTENT VOLUME</dt>
                      <dd className="font-code-dense text-code-dense text-on-surface">{svc.dataPath}</dd>
                    </div>
                  )}
                </>
              )}
              <div>
                <dt className="font-label-sm text-label-sm text-outline">PROFILE</dt>
                <dd className="font-code-dense text-code-dense text-primary">{active.profile}</dd>
              </div>
            </dl>
          </Card>

          <Card title="Dependencies" icon="account_tree" actions={<Link to="/dependency-graph" className="text-[11px] text-primary hover:underline">Graph</Link>}>
            <div className="text-[11px] text-outline mb-1">STARTS AFTER</div>
            {svc.dependsOn.length === 0 ? (
              <div className="text-body-sm text-on-surface-variant mb-space-sm">Nothing: starts first.</div>
            ) : (
              <div className="space-y-1 mb-space-sm">
                {svc.dependsOn.map((d) => (
                  <Link key={d} to={`/services/${encodeURIComponent(d)}`} className="flex items-center justify-between px-2 py-1 rounded bg-surface-container-lowest hover:bg-surface-container-high">
                    <span className="font-code-dense text-code-dense text-on-surface">{d}</span>
                    <StatusPill status={statusOf(d)} />
                  </Link>
                ))}
              </div>
            )}
            <div className="text-[11px] text-outline mb-1">REQUIRED BY</div>
            {dependents.length === 0 ? (
              <div className="text-body-sm text-on-surface-variant">No other service depends on this.</div>
            ) : (
              <div className="space-y-1">
                {dependents.map((d) => (
                  <Link key={d.name} to={`/services/${encodeURIComponent(d.name)}`} className="flex items-center justify-between px-2 py-1 rounded bg-surface-container-lowest hover:bg-surface-container-high">
                    <span className="font-code-dense text-code-dense text-on-surface">{d.name}</span>
                    <StatusPill status={statusOf(d.name)} />
                  </Link>
                ))}
              </div>
            )}
          </Card>

          {git?.isRepo && (
            <Card title="Repository" icon="fork_right" actions={<Link to="/git" className="text-[11px] text-primary hover:underline">Git panel</Link>}>
              <dl className="space-y-1 text-body-sm font-code-dense text-code-dense">
                <div className="flex justify-between"><dt className="text-outline">branch</dt><dd className="text-on-surface">{git.branch}</dd></div>
                <div className="flex justify-between"><dt className="text-outline">sync</dt><dd className="text-on-surface">{git.upstream ? `↑${git.ahead} ↓${git.behind}` : "no upstream"}</dd></div>
                <div className="flex justify-between"><dt className="text-outline">changes</dt><dd className="text-on-surface">{git.changes.length} modified · {git.untracked.length} untracked</dd></div>
                <div className="text-on-surface-variant truncate" title={git.lastCommit}>{git.lastCommit}</div>
              </dl>
            </Card>
          )}
        </div>
      </div>
    </PageBody>
  );
}
