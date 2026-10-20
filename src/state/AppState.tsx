import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { api, isTauri } from "../lib/tauri";
import type { Profile, ServiceRuntime, Settings, StoreData, SystemStats, UpRun, Workspace } from "../lib/types";

type ToastKind = "info" | "success" | "error";
interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
}

interface AppCtx {
  ready: boolean;
  /** Set when the backend is unreachable (e.g. opened in a plain browser). */
  fatal?: string;
  store: StoreData;
  active?: Workspace;
  runtime: Record<string, ServiceRuntime>;
  stats?: SystemStats;
  up: UpRun | null;

  saveWorkspace: (ws: Workspace) => Promise<Workspace | undefined>;
  deleteWorkspace: (id: string) => Promise<void>;
  setActive: (id: string) => Promise<void>;
  setProfile: (p: Profile) => Promise<void>;
  saveSettings: (s: Settings) => Promise<void>;

  startService: (name: string) => Promise<void>;
  stopService: (name: string) => Promise<void>;
  restartService: (name: string) => Promise<void>;
  stopAll: () => Promise<void>;
  startUp: () => Promise<boolean>;
  cancelUp: () => Promise<void>;
  refreshRuntime: () => Promise<void>;
  /** Route a service through the fault proxy (restarts it if it is running). */
  setChaos: (name: string, enabled: boolean) => Promise<boolean>;

  toast: (text: string, kind?: ToastKind) => void;
}

const EMPTY_STORE: StoreData = { workspaces: [], active: null, settings: { healthTimeoutSecs: 60, stopOnExit: true } };

const Ctx = createContext<AppCtx | null>(null);

export function useApp(): AppCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useApp must be used inside <AppStateProvider>");
  return c;
}

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [store, setStore] = useState<StoreData>(EMPTY_STORE);
  const [ready, setReady] = useState(false);
  const [fatal, setFatal] = useState<string>();
  const [runtime, setRuntime] = useState<Record<string, ServiceRuntime>>({});
  const [stats, setStats] = useState<SystemStats>();
  const [up, setUp] = useState<UpRun | null>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);

  const active = useMemo(
    () => store.workspaces.find((w) => w.id === store.active) ?? store.workspaces[0],
    [store.workspaces, store.active],
  );
  const activeId = active?.id;

  const toast = useCallback((text: string, kind: ToastKind = "info") => {
    const id = ++toastId.current;
    setToasts((t) => [...t.slice(-3), { id, kind, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 7000 : 3500);
  }, []);

  const guard = useCallback(
    async <T,>(fn: () => Promise<T>, okMsg?: string): Promise<T | undefined> => {
      try {
        const r = await fn();
        if (okMsg) toast(okMsg, "success");
        return r;
      } catch (e) {
        toast(e instanceof Error ? e.message : String(e), "error");
        return undefined;
      }
    },
    [toast],
  );

  // initial load
  useEffect(() => {
    if (!isTauri()) {
      setFatal("This screen needs the Stackyard desktop shell. Run `npm run tauri dev` (or the installed app).");
      setReady(true);
      return;
    }
    api
      .getStore()
      .then((s) => setStore(s))
      .catch((e) => setFatal(String(e.message ?? e)))
      .finally(() => setReady(true));
  }, []);

  const refreshRuntime = useCallback(async () => {
    if (!activeId) {
      setRuntime({});
      return;
    }
    try {
      const list = await api.runtimeStatus(activeId);
      setRuntime(Object.fromEntries(list.map((r) => [r.name, r])));
    } catch {
      /* transient: keep last known state */
    }
  }, [activeId]);

  // polling: runtime + up pipeline (fast), system stats (slow)
  useEffect(() => {
    if (!ready || fatal) return;
    let stop = false;
    const tick = async () => {
      if (stop || document.hidden) return;
      await refreshRuntime();
      try {
        const u = await api.upStatus();
        if (!stop) setUp(u);
      } catch {
        /* ignore */
      }
    };
    void tick();
    const id = setInterval(tick, 1000);
    return () => {
      stop = true;
      clearInterval(id);
    };
  }, [ready, fatal, refreshRuntime]);

  useEffect(() => {
    if (!ready || fatal) return;
    let stop = false;
    const tick = () => {
      if (document.hidden) return;
      api
        .systemStats()
        .then((s) => !stop && setStats(s))
        .catch(() => {});
    };
    tick();
    const id = setInterval(tick, 3000);
    return () => {
      stop = true;
      clearInterval(id);
    };
  }, [ready, fatal]);

  const value = useMemo<AppCtx>(
    () => ({
      ready,
      fatal,
      store,
      active,
      runtime,
      stats,
      up,
      toast,
      refreshRuntime,

      saveWorkspace: async (ws) => {
        const before = new Set(store.workspaces.map((w) => w.id));
        const s = await guard(() => api.saveWorkspace(ws));
        if (!s) return undefined;
        setStore(s);
        const saved = s.workspaces.find((w) => w.id === ws.id) ?? s.workspaces.find((w) => !before.has(w.id));
        return saved;
      },
      deleteWorkspace: async (id) => {
        const s = await guard(() => api.deleteWorkspace(id), "Workspace deleted");
        if (s) setStore(s);
      },
      setActive: async (id) => {
        const s = await guard(() => api.setActiveWorkspace(id));
        if (s) setStore(s);
      },
      setProfile: async (p) => {
        if (!active || active.profile === p) return;
        const s = await guard(() => api.saveWorkspace({ ...active, profile: p }), `Profile: ${p}. Restart services to apply`);
        if (s) setStore(s);
      },
      saveSettings: async (settings) => {
        const s = await guard(() => api.saveSettings(settings), "Settings saved");
        if (s) setStore(s);
      },

      startService: async (name) => {
        if (!activeId) return;
        await guard(() => api.serviceStart(activeId, name));
        void refreshRuntime();
      },
      stopService: async (name) => {
        if (!activeId) return;
        await guard(() => api.serviceStop(activeId, name));
        void refreshRuntime();
      },
      restartService: async (name) => {
        if (!activeId) return;
        await guard(() => api.serviceRestart(activeId, name), `${name} restarted`);
        void refreshRuntime();
      },
      setChaos: async (name, enabled) => {
        if (!activeId) return false;
        const s = await guard(() => api.chaosEnable(activeId, name, enabled), enabled ? `${name} now runs behind the fault proxy` : `${name} no longer uses the fault proxy`);
        if (!s) return false;
        setStore(s);
        void refreshRuntime();
        return true;
      },
      stopAll: async () => {
        if (!activeId) return;
        await guard(() => api.stopAll(activeId), "Stopping all services");
        void refreshRuntime();
      },
      startUp: async () => {
        if (!activeId) return false;
        const r = await guard(async () => {
          await api.upStart(activeId);
          return true;
        });
        if (r) setUp(await api.upStatus().catch(() => null));
        return !!r;
      },
      cancelUp: async () => {
        await guard(() => api.upCancel());
      },
    }),
    [ready, fatal, store, active, activeId, runtime, stats, up, toast, guard, refreshRuntime],
  );

  return (
    <Ctx.Provider value={value}>
      {children}
      <div className="fixed bottom-8 right-4 z-[100] flex flex-col gap-2 pointer-events-none">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`pointer-events-auto max-w-sm px-3 py-2 rounded-lg border text-body-sm font-body-sm shadow-lg select-text ${
              t.kind === "error"
                ? "bg-error-container/90 border-error/40 text-on-error-container"
                : t.kind === "success"
                  ? "bg-surface-container-high border-secondary/40 text-on-surface"
                  : "bg-surface-container-high border-outline-variant text-on-surface"
            }`}
          >
            {t.text}
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
