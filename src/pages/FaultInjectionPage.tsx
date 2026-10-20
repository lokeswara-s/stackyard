import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Banner, Btn, Card, EmptyState, Field, Icon, PageBody, PageHeader, inputCls } from "../components/ui";
import { api } from "../lib/tauri";
import { NO_FAULTS, type ChaosEvent, type ChaosState, type Faults, type ProbeResult } from "../lib/types";
import { useApp } from "../state/AppState";

interface Preset {
  id: string;
  label: string;
  icon: string;
  hint: string;
  httpOnly?: boolean;
  faults: Partial<Faults>;
}

const PRESETS: Preset[] = [
  {
    id: "outage",
    label: "Dependency outage",
    icon: "cloud_off",
    hint: "Every request fails with 503",
    httpOnly: true,
    faults: { errorRate: 100, status: 503, body: '{"error":"dependency_unavailable","retry_after":30}' },
  },
  { id: "slow", label: "Slow dependency", icon: "hourglass_top", hint: "1.5s ± 0.4s extra latency", faults: { latencyMs: 1500, jitterMs: 400 } },
  { id: "flaky", label: "Flaky network", icon: "network_check", hint: "15% cut, 200ms ± 150ms", faults: { dropRate: 15, latencyMs: 200, jitterMs: 150 } },
  {
    id: "ratelimit",
    label: "Rate limited",
    icon: "speed",
    hint: "30% of requests get 429",
    httpOnly: true,
    faults: { errorRate: 30, status: 429, body: '{"error":"rate_limited","retry_after":30}' },
  },
  { id: "down", label: "Connection loss", icon: "link_off", hint: "Every connection is cut", faults: { dropRate: 100 } },
  { id: "hang", label: "Hangs forever", icon: "timer_off", hint: "Accepts, never answers", faults: { blackhole: true } },
  { id: "slowlink", label: "Slow link", icon: "swap_vert", hint: "32 KB/s and 100ms latency", faults: { throttleKbps: 32, latencyMs: 100 } },
];

const STATUS_CHIPS = [500, 502, 503, 504, 429, 401, 404];

const OUTCOME_UI: Record<ChaosEvent["outcome"], { label: string; cls: string }> = {
  forwarded: { label: "forwarded", cls: "text-secondary" },
  connection: { label: "connection", cls: "text-secondary" },
  passthrough: { label: "websocket / upgrade", cls: "text-primary" },
  error: { label: "error injected", cls: "text-error" },
  dropped: { label: "cut", cls: "text-error" },
  held: { label: "hung", cls: "text-tertiary" },
  "upstream-down": { label: "service down", cls: "text-error" },
};

function describe(f: Faults, http: boolean): string[] {
  const out: string[] = [];
  if (f.blackhole) out.push("hangs");
  if (f.latencyMs > 0) out.push(`+${f.latencyMs}ms${f.jitterMs ? ` ±${f.jitterMs}` : ""}`);
  if (http && f.errorRate > 0) out.push(`${f.errorRate}% → ${f.status}`);
  if (f.dropRate > 0) out.push(`${f.dropRate}% cut`);
  if (f.throttleKbps > 0) out.push(`${f.throttleKbps} KB/s`);
  if (f.pathPrefix && http) out.push(`only ${f.pathPrefix}*`);
  return out;
}

const anyFault = (f: Faults) => f.latencyMs > 0 || f.errorRate > 0 || f.dropRate > 0 || f.blackhole || f.throttleKbps > 0;

function Slider({ label, value, max, unit, step = 1, onChange, hint }: { label: string; value: number; max: number; unit: string; step?: number; onChange: (n: number) => void; hint?: string }) {
  return (
    <div>
      <div className="flex items-baseline justify-between">
        <span className="font-label-sm text-label-sm text-on-surface-variant">{label}</span>
        <span className={`font-code-dense text-code-dense ${value > 0 ? "text-tertiary" : "text-outline"}`}>
          {value}
          {unit}
        </span>
      </div>
      <input type="range" min={0} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))} className="w-full accent-primary" />
      {hint && <div className="text-[10px] text-outline -mt-0.5">{hint}</div>}
    </div>
  );
}

function Counter({ label, value, tone }: { label: string; value: string | number; tone?: string }) {
  return (
    <div className="rounded-lg bg-surface-container-lowest border border-surface-container-highest px-space-sm py-2">
      <div className="text-[10px] font-label-sm uppercase tracking-wider text-outline">{label}</div>
      <div className={`font-headline-sm text-headline-sm ${tone ?? "text-on-surface"}`}>{value}</div>
    </div>
  );
}

export default function FaultInjectionPage() {
  const { active, runtime, setChaos, toast, ready } = useApp();
  const navigate = useNavigate();
  const [states, setStates] = useState<ChaosState[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState<Faults>(NO_FAULTS);
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [probePath, setProbePath] = useState("/");
  const [probing, setProbing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const draftFor = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const wsId = active?.id;

  // Poll live state + traffic.
  useEffect(() => {
    if (!wsId) return;
    let stop = false;
    const tick = async () => {
      try {
        const s = await api.chaosStatus(wsId);
        if (!stop) setStates(s);
      } catch {
        /* backend not reachable: leave the last state */
      }
    };
    void tick();
    const t = setInterval(tick, 1000);
    return () => {
      stop = true;
      clearInterval(t);
    };
  }, [wsId]);

  // Pick a service by default, and load its faults into the editor when the selection changes.
  useEffect(() => {
    if (states.length === 0) return;
    if (!selected || !states.some((s) => s.service === selected)) {
      setSelected((states.find((s) => s.enabled) ?? states.find((s) => s.publicPort) ?? states[0]).service);
      return;
    }
    if (draftFor.current !== selected) {
      draftFor.current = selected;
      setDraft(states.find((s) => s.service === selected)!.faults);
      setProbe(null);
    }
  }, [states, selected]);

  const sel = states.find((s) => s.service === selected);
  const http = sel?.mode !== "tcp";
  const selAlive = sel ? ["running", "starting"].includes(runtime[sel.service]?.status ?? "idle") : false;

  const push = useCallback(
    (f: Faults) => {
      setDraft(f);
      if (!wsId || !selected) return;
      clearTimeout(timer.current);
      timer.current = setTimeout(() => {
        api.chaosSet(wsId, selected, f).catch((e) => toast(e.message, "error"));
      }, 250);
    },
    [wsId, selected, toast],
  );
  const patch = (p: Partial<Faults>) => push({ ...draft, ...p });

  const resetAll = async () => {
    if (!wsId) return;
    await api.chaosReset(wsId).catch((e) => toast(e.message, "error"));
    setDraft(NO_FAULTS);
    toast("All faults cleared", "success");
  };

  const toggleProxy = async (name: string, enabled: boolean) => {
    setBusy(name);
    await setChaos(name, enabled);
    setBusy(null);
  };

  const runProbe = async () => {
    if (!wsId || !selected) return;
    setProbing(true);
    try {
      setProbe(await api.chaosProbe(wsId, selected, probePath));
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
    setProbing(false);
  };

  if (!ready) return null;
  if (!active) {
    return (
      <PageBody>
        <EmptyState icon="bolt" title="No workspace yet" action={<Btn variant="primary" icon="add" onClick={() => navigate("/workspaces/new")}>New workspace</Btn>}>
          Fault injection makes the services of a workspace misbehave on purpose. Create a workspace first.
        </EmptyState>
      </PageBody>
    );
  }

  const faulting = states.filter((s) => s.active && anyFault(s.faults)).length;
  const withPort = states.filter((s) => s.publicPort);

  return (
    <PageBody>
      <PageHeader
        title="Fault Injection"
        subtitle="Make a dependency fail, slow down or hang, and watch how the rest of the stack copes."
        actions={
          <Btn icon="restart_alt" variant={faulting ? "danger" : "secondary"} disabled={!states.some((s) => anyFault(s.faults))} onClick={() => void resetAll()}>
            Clear all faults
          </Btn>
        }
      />

      <Banner icon="info" tone="info">
        A small local proxy takes over a service's port and the service itself moves to a hidden one, so anything already calling <code className="font-code-dense">localhost:&lt;port&gt;</code>{" "}
        goes through it. Faults apply instantly, are never saved, and disappear when you close the app. WebSockets (e.g. Vite HMR) always pass through untouched.
      </Banner>

      {withPort.length === 0 ? (
        <EmptyState icon="bolt" title="No service has a port" action={<Btn icon="edit" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Edit workspace</Btn>}>
          The fault proxy needs a port to sit on. Give a service a port in the workspace editor, then come back.
        </EmptyState>
      ) : (
        <div className="grid gap-space-md" style={{ gridTemplateColumns: "320px minmax(0,1fr)" }}>
          {/* ---------- service list ---------- */}
          <Card title="Services" icon="lan" bodyClass="p-0" className="self-start">
            <ul>
              {states.map((s) => {
                const rt = runtime[s.service];
                const alive = rt && ["running", "starting"].includes(rt.status);
                const on = s.active && anyFault(s.faults);
                const noPort = !s.publicPort;
                return (
                  <li key={s.service} className={`border-b border-surface-container-highest last:border-0 ${selected === s.service ? "bg-primary/10" : "hover:bg-surface-container-high/50"}`}>
                    <button type="button" disabled={noPort} onClick={() => setSelected(s.service)} className="w-full text-left px-space-md py-2.5 disabled:opacity-50">
                      <div className="flex items-center justify-between gap-2">
                        <span className="flex items-center gap-1.5 min-w-0">
                          <Icon name={s.kind === "infra" ? "database" : "terminal"} className="text-[15px] text-on-surface-variant" />
                          <span className="font-headline-sm text-headline-sm text-on-surface truncate">{s.service}</span>
                        </span>
                        <span className="font-code-dense text-code-dense text-on-surface-variant">{s.publicPort ? `:${s.publicPort}` : "no port"}</span>
                      </div>
                      <div className="flex items-center gap-1.5 mt-1 text-[10px] font-label-sm">
                        <span className="px-1 rounded bg-surface-container-highest text-on-surface-variant uppercase">{s.mode}</span>
                        {on ? (
                          <span className="text-tertiary inline-flex items-center gap-0.5">
                            <Icon name="bolt" className="text-[12px]" /> {describe(s.faults, s.mode === "http").join(" · ")}
                          </span>
                        ) : s.active ? (
                          <span className="text-secondary">routed through proxy</span>
                        ) : s.enabled ? (
                          <span className="text-primary">armed: {alive ? "restarting" : "starts with the service"}</span>
                        ) : noPort ? (
                          <span className="text-outline">needs a port</span>
                        ) : (
                          <span className="text-outline">proxy off</span>
                        )}
                      </div>
                    </button>
                  </li>
                );
              })}
            </ul>
          </Card>

          {/* ---------- selected service ---------- */}
          {sel && (
            <div className="space-y-space-md min-w-0">
              <Card
                title={
                  <span className="flex items-center gap-2">
                    {sel.service}
                    <span className="font-code-dense text-code-dense text-on-surface-variant">:{sel.publicPort}</span>
                    {sel.active && sel.upstreamPort && <span className="text-[11px] font-body-sm text-outline">→ service on :{sel.upstreamPort}</span>}
                  </span>
                }
                icon="bolt"
                actions={
                  sel.enabled ? (
                    <Btn variant="ghost" icon="power_off" disabled={busy === sel.service} onClick={() => void toggleProxy(sel.service, false)}>
                      {busy === sel.service ? "Restarting…" : "Stop using proxy"}
                    </Btn>
                  ) : (
                    <Btn variant="primary" icon="bolt" disabled={busy === sel.service} onClick={() => void toggleProxy(sel.service, true)}>
                      {busy === sel.service ? "Restarting…" : "Route through fault proxy"}
                    </Btn>
                  )
                }
              >
                {!sel.enabled ? (
                  <p className="font-body-sm text-body-sm text-on-surface-variant">
                    Faults only work for traffic that goes through the proxy. Turning it on {selAlive ? "restarts this service" : "takes effect the next time this service starts"}, so it can
                    move to a hidden port; clients keep using <b>:{sel.publicPort}</b>.
                  </p>
                ) : (
                  <>
                    {!sel.active && (
                      <p className="font-body-sm text-body-sm text-on-surface-variant mb-space-sm">
                        The proxy is armed and starts together with <b>{sel.service}</b>: start the service (or run Up) to put faults into effect. You can set them up now.
                      </p>
                    )}
                    <div className="flex flex-wrap gap-2">
                      {PRESETS.filter((p) => http || !p.httpOnly).map((p) => (
                        <button
                          key={p.id}
                          type="button"
                          title={p.hint}
                          onClick={() => push({ ...NO_FAULTS, ...p.faults })}
                          className="inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg border border-outline-variant bg-surface-container-high hover:border-primary text-on-surface font-label-md text-label-md"
                        >
                          <Icon name={p.icon} className="text-[16px] text-tertiary" />
                          {p.label}
                        </button>
                      ))}
                      <button type="button" onClick={() => push(NO_FAULTS)} className="inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-on-surface-variant hover:text-on-surface font-label-md text-label-md">
                        <Icon name="restart_alt" className="text-[16px]" /> None
                      </button>
                    </div>
                  </>
                )}
              </Card>

              {sel.enabled && (
                <Card title="Faults" icon="tune" actions={<span className="text-[11px] text-outline">applied live</span>}>
                  <div className="grid grid-cols-2 gap-x-space-lg gap-y-space-md">
                    <Slider label="Added latency" value={draft.latencyMs} max={5000} step={50} unit=" ms" onChange={(n) => patch({ latencyMs: n })} hint={http ? "Before each request reaches the service" : "On every chunk sent to the service"} />
                    <Slider label="Jitter (±)" value={draft.jitterMs} max={2000} step={50} unit=" ms" onChange={(n) => patch({ jitterMs: n })} hint="Random variation around the latency" />
                    {http && <Slider label="Error rate" value={draft.errorRate} max={100} unit="%" onChange={(n) => patch({ errorRate: n })} hint="Answered by the proxy; the service never sees these" />}
                    <Slider label={http ? "Dropped requests" : "Dropped connections"} value={draft.dropRate} max={100} unit="%" onChange={(n) => patch({ dropRate: n })} hint="Connection closed with no response" />
                    <Slider label="Bandwidth limit" value={draft.throttleKbps} max={1024} step={8} unit=" KB/s" onChange={(n) => patch({ throttleKbps: n })} hint="0 = unlimited" />
                    <div className="flex items-center justify-between rounded-lg border border-surface-container-highest bg-surface-container-lowest px-space-md py-2">
                      <div>
                        <div className="font-label-md text-label-md text-on-surface">Hang (blackhole)</div>
                        <div className="text-[10px] text-outline">Accept connections and never answer: timeouts</div>
                      </div>
                      <button
                        type="button"
                        role="switch"
                        aria-checked={draft.blackhole}
                        onClick={() => patch({ blackhole: !draft.blackhole })}
                        className={`w-10 h-5 rounded-full relative transition-colors ${draft.blackhole ? "bg-tertiary" : "bg-surface-container-highest"}`}
                      >
                        <span className={`absolute top-0.5 w-4 h-4 rounded-full bg-on-surface transition-all ${draft.blackhole ? "left-[22px]" : "left-0.5"}`} />
                      </button>
                    </div>
                  </div>

                  {http && (
                    <div className="grid grid-cols-2 gap-x-space-lg gap-y-space-md mt-space-md pt-space-md border-t border-surface-container-highest">
                      <div>
                        <div className="font-label-sm text-label-sm text-on-surface-variant mb-1">Injected status code</div>
                        <div className="flex flex-wrap gap-1.5 items-center">
                          {STATUS_CHIPS.map((c) => (
                            <button
                              key={c}
                              type="button"
                              onClick={() => patch({ status: c })}
                              className={`px-2 py-1 rounded border font-code-dense text-code-dense ${draft.status === c ? "bg-error-container/50 border-error text-error" : "border-outline-variant text-on-surface-variant hover:text-on-surface"}`}
                            >
                              {c}
                            </button>
                          ))}
                          <input
                            className={`${inputCls} !w-16 font-code-dense`}
                            inputMode="numeric"
                            value={draft.status}
                            onChange={(e) => patch({ status: Math.min(599, Number(e.target.value.replace(/\D/g, "").slice(0, 3)) || 0) })}
                            onBlur={() => draft.status < 100 && patch({ status: 503 })}
                          />
                        </div>
                      </div>
                      <Field label="Only for paths starting with" hint="Empty = every request, e.g. /api/payments">
                        <input className={`${inputCls} font-code-dense`} value={draft.pathPrefix} onChange={(e) => patch({ pathPrefix: e.target.value })} placeholder="/" />
                      </Field>
                      <div className="col-span-2">
                        <Field label="Response body for injected errors" hint="Empty = a small JSON error document">
                          <textarea
                            className={`${inputCls} !h-20 py-1.5 font-code-dense resize-y`}
                            value={draft.body}
                            onChange={(e) => patch({ body: e.target.value })}
                            placeholder='{"error":"payment_gateway_down","retry_after":30}'
                          />
                        </Field>
                      </div>
                    </div>
                  )}
                </Card>
              )}

              {sel.enabled && (
                <Card
                  title="Live traffic"
                  icon="monitoring"
                  bodyClass="p-0"
                  actions={
                    http ? (
                      <div className="flex items-center gap-1.5">
                        <input className={`${inputCls} !w-40 font-code-dense`} value={probePath} onChange={(e) => setProbePath(e.target.value)} placeholder="/health" />
                        <Btn icon="send" disabled={probing || !sel.active} onClick={() => void runProbe()}>
                          {probing ? "Sending…" : "Send test request"}
                        </Btn>
                      </div>
                    ) : undefined
                  }
                >
                  {!sel.active || !sel.stats ? (
                    <div className="p-space-lg text-center text-on-surface-variant font-body-sm">Traffic appears here once the service is running behind the proxy.</div>
                  ) : (
                    <>
                      <div className="grid grid-cols-6 gap-space-sm p-space-md">
                        <Counter label={sel.mode === "tcp" ? "Connections" : "Requests"} value={sel.stats.requests} />
                        <Counter label="Per second" value={sel.stats.rps.toFixed(1)} />
                        <Counter label="Injected errors" value={sel.stats.injectedErrors} tone={sel.stats.injectedErrors ? "text-error" : undefined} />
                        <Counter label="Cut" value={sel.stats.dropped} tone={sel.stats.dropped ? "text-error" : undefined} />
                        <Counter label="Hung" value={sel.stats.held} tone={sel.stats.held ? "text-tertiary" : undefined} />
                        <Counter label="Avg added" value={`${sel.stats.requests ? Math.round(sel.stats.addedLatencyMs / sel.stats.requests) : 0} ms`} tone={sel.stats.addedLatencyMs ? "text-tertiary" : undefined} />
                      </div>

                      {probe && (
                        <div className={`mx-space-md mb-space-md rounded-lg border px-space-md py-2 text-[12px] ${probe.error || (probe.status ?? 0) >= 400 ? "border-error/40 bg-error-container/20" : "border-secondary/40 bg-secondary/10"}`}>
                          <div className="flex items-center gap-2 font-code-dense text-code-dense">
                            <Icon name={probe.error || (probe.status ?? 0) >= 400 ? "error" : "check_circle"} className={`text-[16px] ${probe.error || (probe.status ?? 0) >= 400 ? "text-error" : "text-secondary"}`} />
                            <span className="text-on-surface">
                              GET {probePath || "/"} → {probe.error ? probe.error : `HTTP ${probe.status}`}
                            </span>
                            <span className="text-outline ml-auto">{probe.ms} ms</span>
                          </div>
                          {probe.body && <pre className="mt-1 text-on-surface-variant font-code-dense whitespace-pre-wrap break-all select-text">{probe.body}</pre>}
                        </div>
                      )}

                      <table className="w-full text-left">
                        <thead>
                          <tr className="text-[10px] font-label-sm uppercase tracking-wider text-outline border-y border-surface-container-highest">
                            <th className="px-space-md py-1.5 w-24">Time</th>
                            <th className="px-space-md py-1.5 w-16">{http ? "Method" : "Type"}</th>
                            <th className="px-space-md py-1.5">{http ? "Path" : "Target"}</th>
                            <th className="px-space-md py-1.5 w-40">Outcome</th>
                            <th className="px-space-md py-1.5 w-16">Status</th>
                            <th className="px-space-md py-1.5 w-28 text-right">Time taken</th>
                          </tr>
                        </thead>
                        <tbody>
                          {sel.stats.recent.length === 0 ? (
                            <tr>
                              <td colSpan={6} className="px-space-md py-6 text-center text-on-surface-variant font-body-sm">
                                No traffic yet. Use <b>Send test request</b>, or hit <span className="font-code-dense">localhost:{sel.publicPort}</span> from another service.
                              </td>
                            </tr>
                          ) : (
                            [...sel.stats.recent].reverse().map((e) => {
                              const o = OUTCOME_UI[e.outcome];
                              return (
                                <tr key={e.seq} className="border-b border-surface-container-highest last:border-0 font-code-dense text-code-dense">
                                  <td className="px-space-md py-1 text-outline">{new Date(e.ts).toLocaleTimeString()}</td>
                                  <td className="px-space-md py-1 text-on-surface-variant">{e.method}</td>
                                  <td className="px-space-md py-1 text-on-surface truncate max-w-[320px]" title={e.path}>{e.path}</td>
                                  <td className={`px-space-md py-1 ${o.cls}`}>{o.label}</td>
                                  <td className={`px-space-md py-1 ${(e.status ?? 0) >= 400 ? "text-error" : "text-on-surface-variant"}`}>{e.status ?? "–"}</td>
                                  <td className="px-space-md py-1 text-right text-on-surface-variant">
                                    {e.ms} ms{e.addedMs > 0 && <span className="text-tertiary"> (+{e.addedMs})</span>}
                                  </td>
                                </tr>
                              );
                            })
                          )}
                        </tbody>
                      </table>
                    </>
                  )}
                </Card>
              )}
            </div>
          )}
        </div>
      )}
    </PageBody>
  );
}
