import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";

import { Btn, EmptyState, Icon, PageBody } from "../components/ui";
import { useLogs } from "../hooks/useLogs";
import type { LogLine } from "../lib/types";
import { useApp } from "../state/AppState";

const LEVELS = ["error", "warn", "info", "debug"] as const;
const LEVEL_TXT: Record<string, string> = { error: "text-error", warn: "text-tertiary", info: "text-on-surface", debug: "text-outline" };
const LEVEL_BADGE: Record<string, string> = {
  error: "bg-error/15 text-error border-error/40",
  warn: "bg-tertiary/15 text-tertiary border-tertiary/40",
  info: "bg-primary/15 text-primary border-primary/40",
  debug: "bg-surface-container-highest text-outline border-outline-variant",
};
const COLORS = ["text-primary", "text-secondary", "text-tertiary", "text-[#c58bff]", "text-[#ff8bb0]", "text-[#8bd8ff]"];

function tryJson(text: string): unknown | null {
  const t = text.trim();
  const start = t.indexOf("{");
  if (start < 0) return null;
  try {
    return JSON.parse(t.slice(start));
  } catch {
    return null;
  }
}

export default function LogsPage() {
  const { active, runtime, toast } = useApp();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const [paused, setPaused] = useState(false);
  const [follow, setFollow] = useState(true);
  const { lines, clear } = useLogs(active?.id, { paused });
  const [query, setQuery] = useState("");
  const [regex, setRegex] = useState(false);
  // `?level=error` (from a service's error banner) opens the page showing only errors.
  const [levels, setLevels] = useState<Record<string, boolean>>(() => {
    const only = params.get("level");
    return only && ["error", "warn", "info", "debug"].includes(only)
      ? { error: only === "error", warn: only === "warn", info: only === "info", debug: only === "debug" }
      : { error: true, warn: true, info: true, debug: true };
  });
  const [sel, setSel] = useState<LogLine | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  const svcFilter = params.get("service");
  const names = active?.services.map((s) => s.name) ?? [];
  const colorOf = (n: string) => COLORS[Math.max(0, names.indexOf(n)) % COLORS.length];

  const matcher = useMemo(() => {
    const q = query.trim();
    if (!q) return null;
    if (regex) {
      try {
        const r = new RegExp(q, "i");
        return (s: string) => r.test(s);
      } catch {
        return null;
      }
    }
    const lq = q.toLowerCase();
    return (s: string) => s.toLowerCase().includes(lq);
  }, [query, regex]);
  const badRegex = regex && query.trim() && !matcher;

  const filtered = useMemo(
    () => lines.filter((l) => (!svcFilter || l.service === svcFilter) && levels[l.level] && (!matcher || matcher(l.text) || matcher(l.service))),
    [lines, svcFilter, levels, matcher],
  );
  const shown = filtered.slice(-1500);

  const levelCounts = useMemo(() => {
    const c: Record<string, number> = { error: 0, warn: 0, info: 0, debug: 0 };
    lines.forEach((l) => (!svcFilter || l.service === svcFilter) && c[l.level]++);
    return c;
  }, [lines, svcFilter]);

  // density timeline: last 5 minutes in 60 buckets
  const buckets = useMemo(() => {
    const now = Date.now();
    const span = 5 * 60_000;
    const n = 60;
    const b = Array.from({ length: n }, () => ({ total: 0, error: 0 }));
    lines.forEach((l) => {
      const age = now - l.ts;
      if (age < 0 || age > span) return;
      const i = n - 1 - Math.floor((age / span) * n);
      b[i].total++;
      if (l.level === "error") b[i].error++;
    });
    return b;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lines.length]);
  const maxB = Math.max(1, ...buckets.map((x) => x.total));

  useEffect(() => {
    const el = boxRef.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [shown.length, follow]);

  const setService = (n: string | null) => {
    const p = new URLSearchParams(params);
    if (n) p.set("service", n);
    else p.delete("service");
    setParams(p, { replace: true });
  };

  const exportLog = () => {
    const text = filtered.map((l) => `${new Date(l.ts).toISOString()} [${l.service}] ${l.level.toUpperCase()} ${l.text}`).join("\n");
    const url = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${active?.name ?? "workspace"}-logs.txt`;
    a.click();
    URL.revokeObjectURL(url);
    toast(`Exported ${filtered.length} lines`, "success");
  };

  if (!active) {
    return (
      <PageBody>
        <EmptyState icon="segment" title="No active workspace" action={<Btn variant="primary" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          Logs from your services stream here once you start them.
        </EmptyState>
      </PageBody>
    );
  }

  const parsed = sel ? tryJson(sel.text) : null;

  return (
    <div className="flex flex-col" style={{ height: "calc(100vh - 3.5rem - 1.5rem)" }}>
      <div className="px-margin pt-margin pb-space-sm space-y-space-sm">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="font-headline-lg text-headline-lg text-on-surface">Logs</h1>
            <p className="text-body-sm text-on-surface-variant">
              {lines.length.toLocaleString()} lines buffered · {paused ? "paused" : "live"}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Btn icon={follow ? "vertical_align_bottom" : "pause_circle"} variant={follow ? "primary" : "secondary"} onClick={() => setFollow((f) => !f)}>
              Follow tail
            </Btn>
            <Btn icon={paused ? "play_arrow" : "pause"} onClick={() => setPaused((p) => !p)}>
              {paused ? "Resume" : "Pause"}
            </Btn>
            <Btn icon="delete_sweep" onClick={() => void clear()}>Clear</Btn>
            <Btn icon="download" onClick={exportLog} disabled={filtered.length === 0}>Export</Btn>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <button type="button" onClick={() => setService(null)} className={`px-2 py-0.5 rounded-full border text-[11px] font-code-dense ${!svcFilter ? "bg-primary/15 border-primary/50 text-primary" : "border-outline-variant text-on-surface-variant hover:text-on-surface"}`}>
            all services
          </button>
          {names.map((n) => (
            <button key={n} type="button" onClick={() => setService(svcFilter === n ? null : n)} className={`px-2 py-0.5 rounded-full border text-[11px] font-code-dense ${svcFilter === n ? "bg-primary/15 border-primary/50" : "border-outline-variant hover:bg-surface-container-high"} ${colorOf(n)}`}>
              <span className={`inline-block w-1.5 h-1.5 rounded-full mr-1 ${runtime[n]?.status === "running" ? "bg-secondary" : runtime[n]?.status === "failed" ? "bg-error" : "bg-outline"}`} />
              {n}
            </button>
          ))}
          <div className="ml-auto flex items-center gap-1">
            {LEVELS.map((lv) => (
              <button key={lv} type="button" onClick={() => setLevels((s) => ({ ...s, [lv]: !s[lv] }))} className={`px-2 py-0.5 rounded border text-[10px] font-label-sm uppercase ${levels[lv] ? LEVEL_BADGE[lv] : "border-outline-variant text-outline opacity-50"}`}>
                {lv} {levelCounts[lv]}
              </button>
            ))}
          </div>
        </div>

        <div className="flex items-center gap-2">
          <div className="relative flex-1">
            <Icon name="search" className="absolute left-2 top-1/2 -translate-y-1/2 text-[16px] text-outline" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={regex ? "Regex filter, e.g. status=5\\d\\d|timeout" : "Filter text…"}
              className={`w-full h-8 pl-7 pr-2 bg-surface-container-lowest border rounded-lg font-code-dense text-code-dense text-on-surface placeholder:text-outline focus:outline-none select-text ${badRegex ? "border-error" : "border-surface-container-highest focus:border-primary"}`}
            />
          </div>
          <button type="button" onClick={() => setRegex((r) => !r)} title="Regular expression" className={`h-8 px-2 rounded-lg border font-code-dense text-code-dense ${regex ? "bg-primary/15 border-primary/50 text-primary" : "border-outline-variant text-on-surface-variant"}`}>
            .*
          </button>
          <div className="flex items-end gap-px h-8 w-64 px-1 bg-surface-container-lowest border border-surface-container-highest rounded-lg" title="Log volume, last 5 minutes (red = errors)">
            {buckets.map((b, i) => (
              <div key={i} className="flex-1 flex flex-col justify-end h-full py-1">
                <div className={b.error ? "bg-error" : "bg-secondary/60"} style={{ height: `${(b.total / maxB) * 100}%`, minHeight: b.total ? 2 : 0 }} />
              </div>
            ))}
          </div>
        </div>
      </div>

      <div className="flex-1 min-h-0 flex px-margin pb-margin gap-space-md">
        <div ref={boxRef} className="flex-1 min-w-0 overflow-y-auto bg-surface-container-lowest border border-surface-container-highest rounded-lg font-code-dense text-code-dense select-text" onScroll={(e) => {
          const el = e.currentTarget;
          const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
          if (!atBottom && follow) setFollow(false);
        }}>
          {shown.length === 0 && (
            <div className="p-space-xl text-center text-on-surface-variant font-body-sm">
              {lines.length === 0 ? "No logs yet. Start a service (or press Up) and its output streams here." : "No lines match the current filters."}
            </div>
          )}
          {shown.map((l) => (
            <div
              key={l.seq}
              onClick={() => setSel(sel?.seq === l.seq ? null : l)}
              className={`flex gap-2 px-space-sm py-px cursor-pointer hover:bg-surface-container-high ${sel?.seq === l.seq ? "bg-primary/10" : ""} ${l.level === "error" ? "bg-error/5" : ""}`}
            >
              <span className="text-outline shrink-0">{new Date(l.ts).toLocaleTimeString([], { hour12: false })}</span>
              <span className={`shrink-0 w-28 truncate ${colorOf(l.service)}`}>{l.service}</span>
              <span className={`shrink-0 w-10 uppercase ${LEVEL_TXT[l.level]}`}>{l.level}</span>
              <span className={`whitespace-pre-wrap break-all ${l.stream === "system" ? "text-primary" : LEVEL_TXT[l.level]}`}>{l.text}</span>
            </div>
          ))}
        </div>

        {sel && (
          <aside className="w-96 shrink-0 bg-surface-container border border-surface-container-highest rounded-lg flex flex-col min-h-0">
            <header className="flex items-center justify-between px-space-md py-space-sm border-b border-surface-container-highest">
              <h2 className="font-headline-sm text-headline-sm text-on-surface flex items-center gap-1.5"><Icon name="manage_search" className="text-[18px]" /> Log inspector</h2>
              <button type="button" onClick={() => setSel(null)} className="text-outline hover:text-on-surface"><Icon name="close" className="text-[18px]" /></button>
            </header>
            <div className="p-space-md overflow-y-auto space-y-space-sm">
              <dl className="grid grid-cols-2 gap-2 font-code-dense text-code-dense">
                <div><dt className="text-outline">TIME</dt><dd className="text-on-surface">{new Date(sel.ts).toISOString()}</dd></div>
                <div><dt className="text-outline">LEVEL</dt><dd className={LEVEL_TXT[sel.level]}>{sel.level.toUpperCase()}</dd></div>
                <div><dt className="text-outline">SERVICE</dt><dd className={colorOf(sel.service)}>{sel.service}</dd></div>
                <div><dt className="text-outline">STREAM</dt><dd className="text-on-surface">{sel.stream}</dd></div>
              </dl>
              <div>
                <div className="flex items-center justify-between mb-1">
                  <span className="font-label-sm text-label-sm text-outline">{parsed ? "PARSED JSON" : "MESSAGE"}</span>
                  <button
                    type="button"
                    className="text-[11px] text-primary hover:underline"
                    onClick={() => {
                      void navigator.clipboard.writeText(parsed ? JSON.stringify(parsed, null, 2) : sel.text);
                      toast("Copied", "success");
                    }}
                  >
                    copy
                  </button>
                </div>
                <pre className="p-2 rounded bg-surface-container-lowest border border-surface-container-highest font-code-dense text-code-dense text-on-surface whitespace-pre-wrap break-all select-text">
                  {parsed ? JSON.stringify(parsed, null, 2) : sel.text}
                </pre>
              </div>
              <Btn icon="filter_alt" className="w-full" onClick={() => setService(sel.service)}>Only show {sel.service}</Btn>
            </div>
          </aside>
        )}
      </div>
    </div>
  );
}
