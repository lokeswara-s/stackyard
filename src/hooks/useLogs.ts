import { useCallback, useEffect, useRef, useState } from "react";

import { api } from "../lib/tauri";
import type { LogLine } from "../lib/types";

const MAX = 5000;

/** Polls the backend log buffer for a workspace and keeps the newest lines in memory. */
export function useLogs(workspaceId: string | undefined, opts: { paused?: boolean; intervalMs?: number } = {}) {
  const { paused = false, intervalMs = 700 } = opts;
  const [lines, setLines] = useState<LogLine[]>([]);
  const last = useRef(0);
  const wsRef = useRef(workspaceId);

  useEffect(() => {
    wsRef.current = workspaceId;
    last.current = 0;
    setLines([]);
  }, [workspaceId]);

  useEffect(() => {
    if (!workspaceId || paused) return;
    let stop = false;
    const tick = async () => {
      if (document.hidden) return;
      try {
        const page = await api.getLogs(workspaceId, last.current, 2000);
        if (stop || wsRef.current !== workspaceId) return;
        if (page.lastSeq < last.current) {
          // buffer was cleared
          last.current = 0;
          setLines([]);
          return;
        }
        if (page.lines.length) {
          last.current = page.lines[page.lines.length - 1].seq;
          setLines((prev) => {
            const next = prev.concat(page.lines);
            return next.length > MAX ? next.slice(next.length - MAX) : next;
          });
        } else {
          last.current = Math.max(last.current, page.lastSeq);
        }
      } catch {
        /* ignore transient errors */
      }
    };
    void tick();
    const id = setInterval(tick, intervalMs);
    return () => {
      stop = true;
      clearInterval(id);
    };
  }, [workspaceId, paused, intervalMs]);

  const clear = useCallback(async () => {
    if (!workspaceId) return;
    await api.clearLogs(workspaceId).catch(() => {});
    last.current = 0;
    setLines([]);
  }, [workspaceId]);

  return { lines, clear };
}
