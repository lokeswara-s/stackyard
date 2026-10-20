import { useCallback, useEffect, useState } from "react";

import { Banner, Btn, Card, ConfirmBtn, Icon, PageBody, PageHeader } from "../components/ui";
import { api } from "../lib/tauri";
import type { DockerOverview } from "../lib/types";
import { useApp } from "../state/AppState";

type Tab = "containers" | "volumes" | "images";

export default function ContainersPage() {
  const { toast } = useApp();
  const [data, setData] = useState<DockerOverview | null>(null);
  const [tab, setTab] = useState<Tab>("containers");
  const [busy, setBusy] = useState<string | null>(null);
  const [onlyDw, setOnlyDw] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setData(await api.dockerOverview());
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => !document.hidden && !busy && void refresh(), 5000);
    return () => clearInterval(id);
  }, [refresh, busy]);

  const act = async (kind: string, target: string, label: string) => {
    setBusy(target);
    try {
      await api.dockerAction(kind, target);
      toast(`${label}: ${target}`, "success");
      await refresh();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const containers = (data?.containers ?? []).filter((c) => !onlyDw || (c.Names ?? "").startsWith("dw-"));
  const volumes = (data?.volumes ?? []).filter((v) => !onlyDw || (v.Name ?? "").startsWith("dw-"));
  const images = data?.images ?? [];

  const th = "px-space-md py-2 text-[10px] font-label-sm uppercase tracking-wider text-outline";
  const td = "px-space-md py-2 font-code-dense text-code-dense";

  return (
    <PageBody>
      <PageHeader
        title="Containers & Volumes"
        subtitle={data?.available ? `Docker Engine ${data.version}` : "Docker"}
        actions={<Btn icon="refresh" onClick={() => void refresh()}>Refresh</Btn>}
      />

      {data && !data.available && (
        <Banner tone="warn" icon="warning">
          {data.error ?? "Docker is not available."} Infra services (Postgres, Redis, …) need Docker to run.
        </Banner>
      )}

      {data?.available && (
        <>
          <div className="flex items-center justify-between mb-space-md">
            <div className="flex items-center bg-surface-container-lowest p-0.5 rounded-lg border border-surface-container-highest">
              {(["containers", "volumes", "images"] as Tab[]).map((t) => (
                <button key={t} type="button" onClick={() => setTab(t)} className={`px-3 py-1 rounded font-label-sm text-label-sm capitalize ${tab === t ? "bg-surface-container-high text-primary" : "text-on-surface-variant hover:text-on-surface"}`}>
                  {t} <span className="text-outline">{t === "containers" ? containers.length : t === "volumes" ? volumes.length : images.length}</span>
                </button>
              ))}
            </div>
            {tab !== "images" && (
              <label className="flex items-center gap-1.5 text-[11px] text-on-surface-variant cursor-pointer">
                <input type="checkbox" className="accent-primary" checked={onlyDw} onChange={(e) => setOnlyDw(e.target.checked)} /> only Stackyard-managed (dw-*)
              </label>
            )}
          </div>

          <Card bodyClass="p-0">
            <table className="w-full text-left">
              {tab === "containers" && (
                <>
                  <thead><tr className="border-b border-surface-container-highest"><th className={th}>Name</th><th className={th}>Image</th><th className={th}>State</th><th className={th}>Ports</th><th className={th}>Status</th><th className={`${th} text-right`}>Actions</th></tr></thead>
                  <tbody>
                    {containers.map((c) => {
                      const running = c.State === "running";
                      return (
                        <tr key={c.ID} className="border-b border-surface-container-highest last:border-0 hover:bg-surface-container-high/40">
                          <td className={`${td} text-on-surface`}>
                            {c.Names}
                            {(c.Names ?? "").startsWith("dw-") && <span className="ml-2 text-[9px] px-1 rounded bg-primary/15 text-primary">managed</span>}
                          </td>
                          <td className={`${td} text-on-surface-variant`}>{c.Image}</td>
                          <td className={td}>
                            <span className={`inline-flex items-center gap-1 ${running ? "text-secondary" : "text-outline"}`}>
                              <span className={`w-1.5 h-1.5 rounded-full ${running ? "bg-secondary" : "bg-outline"}`} />{c.State}
                            </span>
                          </td>
                          <td className={`${td} text-on-surface-variant max-w-[220px] truncate`} title={c.Ports}>{c.Ports || "–"}</td>
                          <td className={`${td} text-outline`}>{c.Status}</td>
                          <td className={td}>
                            <div className="flex justify-end gap-1">
                              {running ? (
                                <>
                                  <Btn variant="ghost" icon="restart_alt" className="!px-1.5 !py-1" title="Restart" disabled={busy === c.ID} onClick={() => void act("restart", c.ID, "Restarted")} />
                                  <Btn variant="ghost" icon="stop" className="!px-1.5 !py-1" title="Stop" disabled={busy === c.ID} onClick={() => void act("stop", c.ID, "Stopped")} />
                                </>
                              ) : (
                                <Btn variant="ghost" icon="play_arrow" className="!px-1.5 !py-1" title="Start" disabled={busy === c.ID} onClick={() => void act("start", c.ID, "Started")} />
                              )}
                              <ConfirmBtn icon="delete" confirmLabel="Remove?" className="!px-1.5 !py-1" onConfirm={() => void act("rm", c.ID, "Removed")} />
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                    {containers.length === 0 && <tr><td colSpan={6} className="px-space-md py-8 text-center text-on-surface-variant text-body-sm">No containers.</td></tr>}
                  </tbody>
                </>
              )}
              {tab === "volumes" && (
                <>
                  <thead><tr className="border-b border-surface-container-highest"><th className={th}>Name</th><th className={th}>Driver</th><th className={`${th} text-right`}>Actions</th></tr></thead>
                  <tbody>
                    {volumes.map((v) => (
                      <tr key={v.Name} className="border-b border-surface-container-highest last:border-0 hover:bg-surface-container-high/40">
                        <td className={`${td} text-on-surface`}>
                          <Icon name="hard_drive" className="text-[14px] align-middle mr-1.5 text-outline" />{v.Name}
                          {(v.Name ?? "").startsWith("dw-") && <span className="ml-2 text-[9px] px-1 rounded bg-primary/15 text-primary">managed</span>}
                        </td>
                        <td className={`${td} text-on-surface-variant`}>{v.Driver}</td>
                        <td className={td}>
                          <div className="flex justify-end">
                            <ConfirmBtn icon="delete" confirmLabel="Delete data?" className="!px-1.5 !py-1" onConfirm={() => void act("volume_rm", v.Name, "Volume removed")} />
                          </div>
                        </td>
                      </tr>
                    ))}
                    {volumes.length === 0 && <tr><td colSpan={3} className="px-space-md py-8 text-center text-on-surface-variant text-body-sm">No volumes.</td></tr>}
                  </tbody>
                </>
              )}
              {tab === "images" && (
                <>
                  <thead><tr className="border-b border-surface-container-highest"><th className={th}>Repository</th><th className={th}>Tag</th><th className={th}>Size</th><th className={th}>Created</th><th className={`${th} text-right`}>Actions</th></tr></thead>
                  <tbody>
                    {images.map((i) => (
                      <tr key={i.ID + i.Tag} className="border-b border-surface-container-highest last:border-0 hover:bg-surface-container-high/40">
                        <td className={`${td} text-on-surface`}>{i.Repository}</td>
                        <td className={`${td} text-on-surface-variant`}>{i.Tag}</td>
                        <td className={`${td} text-on-surface-variant`}>{i.Size}</td>
                        <td className={`${td} text-outline`}>{i.CreatedSince}</td>
                        <td className={td}>
                          <div className="flex justify-end">
                            <ConfirmBtn icon="delete" confirmLabel="Remove?" className="!px-1.5 !py-1" onConfirm={() => void act("image_rm", i.ID, "Image removed")} />
                          </div>
                        </td>
                      </tr>
                    ))}
                    {images.length === 0 && <tr><td colSpan={5} className="px-space-md py-8 text-center text-on-surface-variant text-body-sm">No images.</td></tr>}
                  </tbody>
                </>
              )}
            </table>
          </Card>
        </>
      )}
      {!data && <div className="text-on-surface-variant text-body-sm">Checking Docker…</div>}
    </PageBody>
  );
}
