import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { BranchDialog, StashDialog } from "../components/GitDialogs";
import { Btn, ConfirmBtn, EmptyState, Icon, PageBody, PageHeader, timeAgo } from "../components/ui";
import { api, revealPath } from "../lib/tauri";
import type { GitStatus, StashEntry, SyncResult } from "../lib/types";
import { useApp } from "../state/AppState";

type RepoState = "clean" | "dirty" | "behind" | "ahead" | "diverged" | "noUpstream" | "notRepo" | "missing";

function stateOf(g: GitStatus): RepoState {
  if (!g.exists) return "missing";
  if (!g.isRepo) return "notRepo";
  if (g.ahead > 0 && g.behind > 0) return "diverged";
  if (g.changes.length + g.untracked.length > 0) return "dirty";
  if (g.behind > 0) return "behind";
  if (g.ahead > 0) return "ahead";
  if (!g.upstream) return "noUpstream";
  return "clean";
}

const STATE_UI: Record<RepoState, { label: string; cls: string }> = {
  clean: { label: "Clean", cls: "text-secondary bg-secondary/10 border-secondary/30" },
  dirty: { label: "Dirty", cls: "text-tertiary bg-tertiary/10 border-tertiary/30" },
  behind: { label: "Behind", cls: "text-primary bg-primary/10 border-primary/30" },
  ahead: { label: "Ahead", cls: "text-primary bg-primary/10 border-primary/30" },
  diverged: { label: "Diverged", cls: "text-error bg-error/10 border-error/30" },
  noUpstream: { label: "No upstream", cls: "text-outline bg-surface-container-highest border-outline-variant" },
  notRepo: { label: "Not a repo", cls: "text-outline bg-surface-container-highest border-outline-variant" },
  missing: { label: "Missing", cls: "text-error bg-error/10 border-error/30" },
};

const FILE_CLS: Record<string, string> = { M: "text-tertiary", A: "text-secondary", D: "text-error", R: "text-primary", U: "text-error", "?": "text-outline" };
const OUTCOME_LABEL: Record<string, string> = {
  synced: "pulled",
  upToDate: "up to date",
  skippedDirty: "skipped (uncommitted changes)",
  diverged: "diverged",
  noUpstream: "no upstream",
  notRepo: "not a repo",
  error: "error",
};

type Tab = "all" | "clean" | "dirty" | "sync";

/** The named stashes of one repo, each with apply / delete. */
function StashList({ path, tick, onAct }: { path: string; tick: number; onAct: (label: string, fn: () => Promise<string>) => void }) {
  const [list, setList] = useState<StashEntry[]>([]);
  useEffect(() => {
    let stop = false;
    api.gitStashList(path).then((l) => !stop && setList(l)).catch(() => !stop && setList([]));
    return () => {
      stop = true;
    };
  }, [path, tick]);
  if (list.length === 0) return <div className="text-[11px] text-outline mb-2">No stashes.</div>;
  return (
    <div className="mb-3 rounded border border-surface-container-highest divide-y divide-surface-container-highest">
      <div className="px-2 py-1 text-[10px] uppercase tracking-wider text-outline font-label-sm">Stashes</div>
      {list.map((s) => (
        <div key={s.index} className="flex items-center gap-2 px-2 py-1">
          <Icon name="inventory" className="text-[14px] text-outline" />
          <span className="flex-1 min-w-0 font-code-dense text-code-dense text-on-surface truncate select-text" title={s.message}>
            {s.message.replace(/^(On|WIP on) [^:]+: /, "")}
          </span>
          <span className="text-[10px] text-outline">{s.age}</span>
          <Btn variant="ghost" icon="unarchive" className="!px-2 !py-0.5" title="Apply this stash and remove it" onClick={() => onAct("Stash pop", () => api.gitStashPop(path, s.index))}>
            Pop
          </Btn>
          <ConfirmBtn icon="delete" className="!px-1.5 !py-0.5" title="Delete this stash" confirmLabel="Delete?" onConfirm={() => onAct("Drop stash", () => api.gitStashDrop(path, s.index))} />
        </div>
      ))}
    </div>
  );
}

export default function GitMultiRepoPage() {
  const { active, toast } = useApp();
  const navigate = useNavigate();
  const [status, setStatus] = useState<Record<string, GitStatus>>({});
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<Record<string, string>>({});
  const [tab, setTab] = useState<Tab>("all");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [diff, setDiff] = useState<{ repo: string; file: string; text: string } | null>(null);
  const [summary, setSummary] = useState<{ title: string; results: SyncResult[]; at: number } | null>(null);
  const [stashFor, setStashFor] = useState<string | null>(null);
  const [branchFor, setBranchFor] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  const repos = useMemo(() => {
    const map = new Map<string, string[]>();
    (active?.services ?? []).forEach((s) => {
      if (s.kind === "app" && s.repoPath) map.set(s.repoPath, [...(map.get(s.repoPath) ?? []), s.name]);
    });
    return [...map.entries()].map(([path, services]) => ({ path, services }));
  }, [active]);
  const paths = useMemo(() => repos.map((r) => r.path), [repos]);

  const refresh = useCallback(
    async (silent = false) => {
      if (paths.length === 0) return;
      if (!silent) setLoading(true);
      try {
        const list = await api.gitStatus(paths);
        setStatus((prev) => ({ ...prev, ...Object.fromEntries(list.map((g) => [g.path, g])) }));
      } catch (e) {
        toast(e instanceof Error ? e.message : String(e), "error");
      } finally {
        setLoading(false);
      }
    },
    [paths, toast],
  );

  useEffect(() => {
    void refresh();
    const id = setInterval(() => !document.hidden && void refresh(true), 20000);
    return () => clearInterval(id);
  }, [refresh]);

  const run = async (label: string, targets: string[], fn: (p: string[]) => Promise<SyncResult[]>) => {
    setBusy((b) => ({ ...b, ...Object.fromEntries(targets.map((t) => [t, label])) }));
    try {
      const results = await fn(targets);
      setSummary({ title: label, results, at: Date.now() });
      await refresh(true);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy((b) => {
        const n = { ...b };
        targets.forEach((t) => delete n[t]);
        return n;
      });
    }
  };

  const single = async (path: string, label: string, fn: () => Promise<string>) => {
    setBusy((b) => ({ ...b, [path]: label }));
    try {
      const out = await fn();
      toast(out || `${label} done`, "success");
      setTick((t) => t + 1);
      await refresh(true);
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy((b) => {
        const n = { ...b };
        delete n[path];
        return n;
      });
    }
  };

  const rows = repos
    .map((r) => ({ ...r, g: status[r.path] }))
    .filter((r) => {
      const q = query.trim().toLowerCase();
      if (q && !(r.path.toLowerCase().includes(q) || r.services.some((s) => s.toLowerCase().includes(q)) || (r.g?.branch ?? "").toLowerCase().includes(q))) return false;
      if (!r.g) return tab === "all";
      const st = stateOf(r.g);
      if (tab === "clean") return st === "clean";
      if (tab === "dirty") return st === "dirty";
      if (tab === "sync") return st === "behind" || st === "diverged" || st === "ahead";
      return true;
    });

  const count = (t: Tab) =>
    repos.filter((r) => {
      const g = status[r.path];
      if (t === "all") return true;
      if (!g) return false;
      const st = stateOf(g);
      return t === "clean" ? st === "clean" : t === "dirty" ? st === "dirty" : st === "behind" || st === "diverged" || st === "ahead";
    }).length;

  if (!active) {
    return (
      <PageBody>
        <EmptyState icon="fork_right" title="No active workspace" action={<Btn variant="primary" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          Repositories appear here once you add them to a workspace.
        </EmptyState>
      </PageBody>
    );
  }

  const counts = summary?.results.reduce<Record<string, number>>((a, r) => ((a[r.outcome] = (a[r.outcome] ?? 0) + 1), a), {}) ?? {};

  return (
    <PageBody>
      <PageHeader
        title="Git & Multi-repo"
        subtitle="Safe sync fast-forwards clean repos only. Anything with local changes or divergence is left untouched."
        actions={
          <>
            <Btn icon="refresh" onClick={() => void refresh()} disabled={loading}>
              {loading ? "Refreshing…" : "Refresh"}
            </Btn>
            <Btn icon="cloud_download" disabled={repos.length === 0} onClick={() => void run("Fetch all", paths, api.gitFetch)}>
              Fetch all
            </Btn>
            <Btn icon="download" variant="success" disabled={repos.length === 0} onClick={() => void run("Pull all (safe)", paths, api.gitPullSafe)}>
              Pull all (safe)
            </Btn>
          </>
        }
      />

      {summary && (
        <div className="mb-space-md px-space-md py-space-sm rounded-lg border border-surface-container-highest bg-surface-container flex items-start gap-3">
          <Icon name="task_alt" className="text-secondary text-[22px] mt-0.5" />
          <div className="flex-1">
            <div className="font-headline-sm text-headline-sm text-on-surface">
              {summary.title} <span className="text-outline font-normal text-body-sm">· {timeAgo(summary.at)}</span>
            </div>
            <div className="text-[11px] text-on-surface-variant mt-0.5 select-text">
              {Object.entries(counts).map(([k, n]) => `${n} ${OUTCOME_LABEL[k] ?? k}`).join(" · ")}
            </div>
            <ul className="mt-1 space-y-0.5 text-[11px] font-code-dense">
              {summary.results
                .filter((r) => r.outcome !== "upToDate" && r.outcome !== "synced")
                .map((r) => (
                  <li key={r.path} className="text-tertiary select-text">
                    {r.path.split(/[\\/]/).pop()}: {r.message}
                  </li>
                ))}
            </ul>
          </div>
          <button type="button" onClick={() => setSummary(null)} className="text-outline hover:text-on-surface">
            <Icon name="close" className="text-[18px]" />
          </button>
        </div>
      )}

      {repos.length === 0 ? (
        <EmptyState icon="fork_right" title="No repositories in this workspace" action={<Btn variant="primary" onClick={() => navigate(`/workspaces/${active.id}/edit`)}>Add repositories</Btn>}>
          Add app services with a folder and they will be tracked here.
        </EmptyState>
      ) : (
        <>
          <div className="flex items-center justify-between mb-space-md">
            <div className="flex items-center bg-surface-container-lowest p-0.5 rounded-lg border border-surface-container-highest">
              {([["all", "All"], ["clean", "Clean"], ["dirty", "Dirty"], ["sync", "Needs sync"]] as [Tab, string][]).map(([id, label]) => (
                <button key={id} type="button" onClick={() => setTab(id)} className={`px-3 py-1 rounded font-label-sm text-label-sm ${tab === id ? "bg-surface-container-high text-primary" : "text-on-surface-variant hover:text-on-surface"}`}>
                  {label} <span className="text-outline">{count(id)}</span>
                </button>
              ))}
            </div>
            <div className="relative w-64">
              <Icon name="search" className="absolute left-2 top-1/2 -translate-y-1/2 text-[16px] text-outline" />
              <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Filter by repo, service, branch…" className="w-full h-8 pl-7 pr-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text" />
            </div>
          </div>

          <div className="bg-surface-container border border-surface-container-highest rounded-lg overflow-hidden">
            <table className="w-full text-left">
              <thead>
                <tr className="bg-surface-container-low text-[10px] font-label-sm uppercase tracking-wider text-outline border-b border-surface-container-highest">
                  <th className="px-space-md py-2 w-8" />
                  <th className="px-space-md py-2">Repository</th>
                  <th className="px-space-md py-2">Branch</th>
                  <th className="px-space-md py-2">Sync</th>
                  <th className="px-space-md py-2">Changes</th>
                  <th className="px-space-md py-2">Last fetch</th>
                  <th className="px-space-md py-2">State</th>
                  <th className="px-space-md py-2 text-right">Actions</th>
                </tr>
              </thead>
              <tbody>
                {rows.map(({ path, services, g }) => {
                  const st = g ? stateOf(g) : null;
                  const name = path.split(/[\\/]/).filter(Boolean).pop() ?? path;
                  const b = busy[path];
                  const nChanges = g ? g.changes.length + g.untracked.length : 0;
                  const isOpen = open[path];
                  return (
                    <Fragment key={path}>
                      <tr className="border-b border-surface-container-highest hover:bg-surface-container-high/50">
                        <td className="px-space-md py-2">
                          <button type="button" disabled={!g?.isRepo} onClick={() => setOpen((o) => ({ ...o, [path]: !o[path] }))} className="text-outline hover:text-on-surface disabled:opacity-30">
                            <Icon name={isOpen ? "expand_more" : "chevron_right"} className="text-[18px]" />
                          </button>
                        </td>
                        <td className="px-space-md py-2">
                          <div className="font-headline-sm text-headline-sm text-on-surface">{name}</div>
                          <div className="font-code-dense text-code-dense text-outline truncate max-w-xs" title={path}>{path}</div>
                          <div className="flex gap-1 mt-0.5">
                            {services.map((s) => (
                              <span key={s} className="px-1 rounded bg-surface-container-highest text-[10px] font-code-dense text-on-surface-variant">{s}</span>
                            ))}
                          </div>
                        </td>
                        <td className="px-space-md py-2 font-code-dense text-code-dense text-on-surface">
                          {g?.isRepo ? (
                            <button type="button" className="inline-flex items-center gap-1 hover:text-primary" title="Switch branch" disabled={!!b} onClick={() => setBranchFor(path)}>
                              {g.branch}
                              <Icon name="unfold_more" className="text-[14px] text-outline" />
                            </button>
                          ) : (
                            "–"
                          )}
                        </td>
                        <td className="px-space-md py-2 font-code-dense text-code-dense">
                          {g?.isRepo ? (g.upstream ? <span className="text-on-surface">↑{g.ahead} ↓{g.behind}</span> : <span className="text-outline">no upstream</span>) : "–"}
                        </td>
                        <td className="px-space-md py-2 font-code-dense text-code-dense text-on-surface-variant">
                          {g?.isRepo ? (nChanges ? `${g.changes.length} modified · ${g.untracked.length} untracked` : "none") : "–"}
                        </td>
                        <td className="px-space-md py-2 font-code-dense text-code-dense text-outline">{g?.isRepo ? timeAgo(g.lastFetchMs) : "–"}</td>
                        <td className="px-space-md py-2">
                          {st ? (
                            <span className={`inline-flex px-1.5 py-0.5 rounded border text-[10px] font-label-sm ${STATE_UI[st].cls}`} title={g?.error ?? undefined}>
                              {STATE_UI[st].label}
                            </span>
                          ) : (
                            <span className="text-outline text-[11px]">…</span>
                          )}
                        </td>
                        <td className="px-space-md py-2">
                          <div className="flex items-center justify-end gap-1">
                            {b && <span className="text-[10px] text-primary mr-1 animate-pulse">{b}…</span>}
                            <Btn variant="ghost" icon="cloud_download" className="!px-1.5 !py-1" title="Fetch" disabled={!g?.isRepo || !!b} onClick={() => void run("Fetch", [path], api.gitFetch)} />
                            <Btn variant="ghost" icon="download" className="!px-1.5 !py-1" title="Pull (fast-forward only)" disabled={!g?.isRepo || !!b} onClick={() => void run("Pull (safe)", [path], api.gitPullSafe)} />
                            <Btn variant="ghost" icon="swap_horiz" className="!px-1.5 !py-1" title="Switch branch" disabled={!g?.isRepo || !!b} onClick={() => setBranchFor(path)} />
                            {nChanges > 0 && <Btn variant="ghost" icon="inventory" className="!px-1.5 !py-1" title="Stash changes…" disabled={!!b} onClick={() => setStashFor(path)} />}
                            <Btn variant="ghost" icon="folder_open" className="!px-1.5 !py-1" title="Show in file explorer" onClick={() => void revealPath(path)} />
                          </div>
                        </td>
                      </tr>
                      {isOpen && g?.isRepo && (
                        <tr className="bg-surface-container-lowest border-b border-surface-container-highest">
                          <td />
                          <td colSpan={7} className="px-space-md py-space-md">
                            <div className="flex items-center justify-between mb-2">
                              <div className="text-[11px] text-on-surface-variant font-code-dense">
                                {g.lastCommit} {g.remote && <span className="text-outline">· {g.remote}</span>}
                              </div>
                              {nChanges > 0 && (
                                <Btn variant="ghost" icon="inventory" className="!px-2 !py-0.5" onClick={() => setStashFor(path)}>
                                  Stash…
                                </Btn>
                              )}
                            </div>
                            <StashList path={path} tick={tick} onAct={(label, fn) => void single(path, label, fn)} />
                            {nChanges === 0 ? (
                              <div className="text-body-sm text-on-surface-variant">Working tree clean.</div>
                            ) : (
                              <div className="space-y-0.5 font-code-dense text-code-dense">
                                {g.changes.map((c) => {
                                  const letter = c.worktree !== "." ? c.worktree : c.index;
                                  return (
                                    <div key={c.path} className="flex items-center gap-3">
                                      <span className={`w-4 text-center font-semibold ${FILE_CLS[letter] ?? "text-on-surface"}`}>{letter}</span>
                                      <span className="text-on-surface flex-1 select-text">{c.path}</span>
                                      {c.added != null && <span className="text-secondary">+{c.added}</span>}
                                      {c.removed != null && <span className="text-error">−{c.removed}</span>}
                                      <button
                                        type="button"
                                        className="text-primary hover:underline"
                                        onClick={async () => {
                                          if (diff?.repo === path && diff.file === c.path) return setDiff(null);
                                          try {
                                            setDiff({ repo: path, file: c.path, text: await api.gitDiffFile(path, c.path) });
                                          } catch (e) {
                                            toast(e instanceof Error ? e.message : String(e), "error");
                                          }
                                        }}
                                      >
                                        diff
                                      </button>
                                    </div>
                                  );
                                })}
                                {g.untracked.map((p) => (
                                  <div key={p} className="flex items-center gap-3">
                                    <span className="w-4 text-center text-outline">?</span>
                                    <span className="text-on-surface-variant select-text">{p}</span>
                                    <span className="text-outline">untracked</span>
                                  </div>
                                ))}
                              </div>
                            )}
                            {diff?.repo === path && (
                              <pre className="mt-3 max-h-72 overflow-auto p-2 rounded bg-surface-container-low border border-surface-container-highest font-code-dense text-code-dense select-text whitespace-pre">
                                {diff.text.split("\n").map((l, i) => (
                                  <div key={i} className={l.startsWith("+") && !l.startsWith("+++") ? "text-secondary" : l.startsWith("-") && !l.startsWith("---") ? "text-error" : l.startsWith("@@") ? "text-primary" : "text-on-surface-variant"}>
                                    {l || " "}
                                  </div>
                                ))}
                              </pre>
                            )}
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={8} className="px-space-md py-8 text-center text-on-surface-variant text-body-sm">No repositories match.</td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </>
      )}

      {stashFor && status[stashFor] && (
        <StashDialog
          path={stashFor}
          repoName={stashFor.split(/[\\/]/).filter(Boolean).pop() ?? stashFor}
          git={status[stashFor]}
          onClose={() => setStashFor(null)}
          onDone={(m) => {
            setStashFor(null);
            toast(m, "success");
            setTick((t) => t + 1);
            void refresh(true);
          }}
        />
      )}
      {branchFor && status[branchFor] && (
        <BranchDialog
          path={branchFor}
          repoName={branchFor.split(/[\\/]/).filter(Boolean).pop() ?? branchFor}
          git={status[branchFor]}
          onClose={() => setBranchFor(null)}
          onDone={(m) => {
            setBranchFor(null);
            toast(m, "success");
            setTick((t) => t + 1);
            void refresh(true);
          }}
        />
      )}
    </PageBody>
  );
}
