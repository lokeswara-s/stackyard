import { useCallback, useEffect, useState } from "react";

import { api, isTauri, revealPath } from "../lib/tauri";
import type { StorageItem, StorageReport } from "../lib/types";
import { useApp } from "../state/AppState";
import { Btn, Card, ConfirmBtn, Icon } from "./ui";

export function formatBytes(n?: number | null): string {
  if (n == null) return "–";
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
}

function sizeOf(i: StorageItem): string {
  if (i.sizeText) return i.sizeText;
  if (i.bytes != null) return formatBytes(i.bytes);
  if (i.count != null) return `${i.count} item${i.count === 1 ? "" : "s"}`;
  return "–";
}

/** Settings card: what Stackyard keeps on disk / in Docker, and buttons to wipe it. */
export function StorageCard() {
  const { toast } = useApp();
  const [report, setReport] = useState<StorageReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!isTauri()) return;
    setLoading(true);
    try {
      setReport(await api.storageReport());
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setLoading(false);
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  const clean = async (ids: string[], label: string) => {
    setBusy(label);
    try {
      const r = await api.storageClean(ids);
      toast(`${r.messages.join(" · ")}${r.freedBytes > 0 ? ` · ${formatBytes(r.freedBytes)} freed` : ""}`, "success");
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy(null);
      await load();
    }
  };

  const low = !!report && report.diskFreeBytes != null && report.diskFreeBytes < report.minFreeBytes * 2;
  const routine = report?.items.filter((i) => i.routine) ?? [];
  const routineBlocked = routine.find((i) => i.blocked)?.blocked;

  return (
    <Card
      title="Storage & cleanup"
      icon="cleaning_services"
      actions={
        <div className="flex items-center gap-1.5">
          <Btn variant="ghost" icon="refresh" disabled={loading || !!busy} onClick={() => void load()}>
            {loading ? "Measuring…" : "Refresh"}
          </Btn>
          <Btn variant="primary" icon={busy === "all" ? "progress_activity" : "delete_sweep"} disabled={!report || !!busy || !!routineBlocked} title={routineBlocked ?? "Pipeline run files, caches and leftover containers"} onClick={() => void clean(routine.map((i) => i.id), "all")}>
            {busy === "all" ? "Cleaning…" : "Clean Stackyard files"}
          </Btn>
        </div>
      }
    >
      {!report ? (
        <div className="text-body-sm text-on-surface-variant">{loading ? "Measuring what Stackyard keeps on disk…" : "Not available."}</div>
      ) : (
        <div className="space-y-space-md">
          <div className="flex flex-wrap items-center gap-x-space-lg gap-y-1 text-[12px] text-on-surface-variant">
            <span>
              App data folder: <span className="text-on-surface font-code-dense">{formatBytes(report.dataBytes)}</span>
            </span>
            <span className={low ? "text-error" : ""}>
              Disk free: <span className="font-code-dense">{formatBytes(report.diskFreeBytes)}</span>
              {low && " · low: pipelines will not start below " + formatBytes(report.minFreeBytes)}
            </span>
            <button type="button" className="inline-flex items-center gap-1 text-primary hover:underline truncate max-w-md" title={report.dataDir} onClick={() => void revealPath(report.dataDir)}>
              <Icon name="folder_open" className="text-[14px]" />
              {report.dataDir}
            </button>
          </div>

          <ul className="divide-y divide-surface-container-highest border border-surface-container-highest rounded-lg">
            {report.items.map((i) => (
              <li key={i.id} className="flex items-center gap-3 px-space-md py-space-sm">
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-label-md text-label-md text-on-surface">{i.label}</span>
                    {i.dockerWide && <span className="px-1.5 rounded bg-tertiary/15 text-tertiary text-[10px] font-label-sm">all of Docker</span>}
                    {i.danger && <span className="px-1.5 rounded bg-error/15 text-error text-[10px] font-label-sm">deletes data</span>}
                  </div>
                  <div className="text-[11px] text-on-surface-variant">{i.description}</div>
                  {i.blocked && <div className="text-[11px] text-tertiary mt-0.5">{i.blocked}</div>}
                </div>
                <span className="font-code-dense text-code-dense text-on-surface whitespace-nowrap">{sizeOf(i)}</span>
                {i.danger || i.dockerWide ? (
                  <ConfirmBtn variant="ghost" icon={busy === i.id ? "progress_activity" : "delete"} disabled={!!busy || !!i.blocked} confirmLabel="Really delete?" onConfirm={() => void clean([i.id], i.id)}>
                    Clean
                  </ConfirmBtn>
                ) : (
                  <Btn variant="ghost" icon={busy === i.id ? "progress_activity" : "delete"} disabled={!!busy || !!i.blocked || (i.bytes === 0 && !i.count)} onClick={() => void clean([i.id], i.id)}>
                    Clean
                  </Btn>
                )}
              </li>
            ))}
          </ul>
          <p className="text-[11px] text-outline">
            Your workspaces and settings are never touched. Downloaded runner images are managed on the Plugins page. Pipeline caches are also pruned automatically (older than 14 days, or above 5 GB) each time Stackyard starts.
          </p>
        </div>
      )}
    </Card>
  );
}
