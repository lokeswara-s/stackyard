import { useCallback, useEffect, useMemo, useState } from "react";

import { Btn, Card, ConfirmBtn, Icon, PageBody, PageHeader } from "../components/ui";
import { api, openExternal } from "../lib/tauri";
import type { Listener } from "../lib/types";
import { useApp } from "../state/AppState";

export default function PortsPage() {
  const { active, runtime, toast } = useApp();
  const [listeners, setListeners] = useState<Listener[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [onlyDev, setOnlyDev] = useState(true);

  const refresh = useCallback(async () => {
    try {
      setListeners(await api.listListeners());
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setLoading(false);
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => !document.hidden && void refresh(), 4000);
    return () => clearInterval(id);
  }, [refresh]);

  const kill = async (l: Listener) => {
    try {
      if (l.container) {
        await api.dockerAction("stop", l.container);
        toast(`Stopped container ${l.container}`, "success");
      } else {
        await api.killPid(l.pid);
        toast(`Killed ${l.process} (PID ${l.pid})`, "success");
      }
      await refresh();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  };

  const byPort = useMemo(() => {
    const m = new Map<number, Listener[]>();
    listeners.forEach((l) => m.set(l.port, [...(m.get(l.port) ?? []), l]));
    return m;
  }, [listeners]);

  // The port that matters is the one the service really listens on; fall back to the configured one.
  const portOf = (s: { name: string; port?: number | null }) => runtime[s.name]?.livePort ?? s.port ?? null;
  const declared = (active?.services ?? [])
    .filter((s) => portOf(s) != null)
    .sort((a, b) => (portOf(a) ?? 0) - (portOf(b) ?? 0));
  const managedPids = new Set(Object.values(runtime).map((r) => r.pid).filter(Boolean));
  const q = query.trim().toLowerCase();
  const all = listeners.filter((l) => (!onlyDev || l.port >= 1024) && (!q || String(l.port).includes(q) || l.process.toLowerCase().includes(q)));
  const declaredPorts = new Set(declared.map((d) => portOf(d)));

  return (
    <PageBody>
      <PageHeader
        title="Ports"
        subtitle="What this workspace needs, and what is actually listening on this machine right now."
        actions={<Btn icon="refresh" onClick={() => void refresh()}>{loading ? "Scanning…" : "Rescan"}</Btn>}
      />

      <Card title="Workspace ports" icon="lan" bodyClass="p-0" className="mb-space-md">
        {declared.length === 0 ? (
          <div className="p-space-lg text-center text-on-surface-variant text-body-sm">No ports configured. Set a port on a service in the workspace editor.</div>
        ) : (
          <table className="w-full text-left">
            <thead>
              <tr className="text-[10px] font-label-sm uppercase tracking-wider text-outline border-b border-surface-container-highest">
                <th className="px-space-md py-2">Service</th>
                <th className="px-space-md py-2">Port</th>
                <th className="px-space-md py-2">State</th>
                <th className="px-space-md py-2">Listener</th>
                <th className="px-space-md py-2 text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              {declared.map((s) => {
                const port = portOf(s)!;
                const owners = byPort.get(port) ?? [];
                const rt = runtime[s.name];
                const ours = rt?.pid != null && (owners.some((o) => o.pid === rt.pid) || rt.portOpen);
                const foreign = owners.filter((o) => !managedPids.has(o.pid));
                const conflict = foreign.length > 0 && !ours && !["running", "starting"].includes(rt?.status ?? "idle");
                const dupes = declared.filter((d) => portOf(d) === port).length > 1;
                return (
                  <tr key={s.name} className="border-b border-surface-container-highest last:border-0">
                    <td className="px-space-md py-2 font-headline-sm text-headline-sm text-on-surface">{s.name}</td>
                    <td className="px-space-md py-2 font-code-dense text-code-dense text-on-surface">
                      :{port}
                      {s.port == null && <span className="ml-1.5 text-[10px] text-outline">auto-detected</span>}
                      {rt?.portMismatch && <span className="ml-1.5 text-[10px] text-tertiary">configured :{s.port}</span>}
                    </td>
                    <td className="px-space-md py-2">
                      {dupes ? (
                        <span className="text-error text-[11px]">declared twice</span>
                      ) : conflict ? (
                        <span className="inline-flex items-center gap-1 text-error text-[11px]"><Icon name="error" className="text-[14px]" />conflict</span>
                      ) : owners.length > 0 ? (
                        <span className="inline-flex items-center gap-1 text-secondary text-[11px]"><Icon name="check_circle" className="text-[14px]" />in use by this service</span>
                      ) : (
                        <span className="text-outline text-[11px]">free</span>
                      )}
                    </td>
                    <td className="px-space-md py-2 font-code-dense text-code-dense text-on-surface-variant">
                      {owners.length ? owners.map((o) => (o.container ? `container ${o.container}` : `${o.process} (PID ${o.pid})`)).join(", ") : "–"}
                    </td>
                    <td className="px-space-md py-2">
                      <div className="flex justify-end gap-1">
                        {owners.length > 0 && s.kind === "app" && (
                          <Btn variant="ghost" icon="open_in_new" className="!px-2 !py-1" onClick={() => void openExternal(`http://localhost:${port}`)}>Open</Btn>
                        )}
                        {conflict && (
                          <ConfirmBtn
                            icon="bolt"
                            confirmLabel={foreign[0].container ? `Stop ${foreign[0].container}?` : `Kill PID ${foreign[0].pid}?`}
                            onConfirm={() => void kill(foreign[0])}
                            className="!px-2 !py-1"
                          >
                            Free port
                          </ConfirmBtn>
                        )}
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </Card>

      <Card
        title="Listening on this machine"
        icon="router"
        bodyClass="p-0"
        actions={
          <div className="flex items-center gap-3">
            <label className="flex items-center gap-1.5 text-[11px] text-on-surface-variant cursor-pointer">
              <input type="checkbox" className="accent-primary" checked={onlyDev} onChange={(e) => setOnlyDev(e.target.checked)} /> ports ≥ 1024
            </label>
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Filter port / process" className="h-7 w-48 px-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text" />
          </div>
        }
      >
        <table className="w-full text-left">
          <thead>
            <tr className="text-[10px] font-label-sm uppercase tracking-wider text-outline border-b border-surface-container-highest">
              <th className="px-space-md py-2">Port</th>
              <th className="px-space-md py-2">Process</th>
              <th className="px-space-md py-2">PID</th>
              <th className="px-space-md py-2">Belongs to</th>
              <th className="px-space-md py-2 text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            {all.map((l) => {
              const svc = declaredPorts.has(l.port) ? declared.find((d) => portOf(d) === l.port) : undefined;
              const managed = managedPids.has(l.pid);
              return (
                <tr key={`${l.port}-${l.pid}`} className="border-b border-surface-container-highest last:border-0 hover:bg-surface-container-high/40">
                  <td className="px-space-md py-1.5 font-code-dense text-code-dense text-on-surface">:{l.port}</td>
                  <td className="px-space-md py-1.5 font-code-dense text-code-dense text-on-surface-variant">
                    {l.container ? (
                      <>
                        <span className="text-on-surface">{l.container}</span> <span className="text-outline">(container via {l.process})</span>
                      </>
                    ) : (
                      l.process
                    )}
                  </td>
                  <td className="px-space-md py-1.5 font-code-dense text-code-dense text-outline">{l.pid}</td>
                  <td className="px-space-md py-1.5 text-[11px]">{svc ? <span className="text-primary">{svc.name}</span> : <span className="text-outline">{managed ? "workspace" : "other"}</span>}</td>
                  <td className="px-space-md py-1.5 text-right">
                    {l.container ? (
                      <ConfirmBtn icon="stop" confirmLabel="Stop container?" onConfirm={() => void kill(l)} className="!px-2 !py-0.5">Stop container</ConfirmBtn>
                    ) : (
                      <ConfirmBtn icon="close" confirmLabel="Kill process?" onConfirm={() => void kill(l)} className="!px-2 !py-0.5" disabled={/docker|wsl|vmmem/i.test(l.process)}>Kill</ConfirmBtn>
                    )}
                  </td>
                </tr>
              );
            })}
            {all.length === 0 && (
              <tr><td colSpan={5} className="px-space-md py-8 text-center text-on-surface-variant text-body-sm">{loading ? "Scanning…" : "Nothing listening matches."}</td></tr>
            )}
          </tbody>
        </table>
      </Card>
    </PageBody>
  );
}
