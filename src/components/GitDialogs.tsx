import { useEffect, useMemo, useState } from "react";

import { api } from "../lib/tauri";
import type { BranchInfo, Branches, GitStatus } from "../lib/types";
import { Modal } from "./Modal";
import { Btn, Field, Icon, inputCls } from "./ui";

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

function ErrorBox({ text }: { text: string }) {
  return <pre className="mt-space-sm p-2 rounded bg-error-container/30 border border-error/40 text-error text-[12px] whitespace-pre-wrap select-text font-body-sm">{text}</pre>;
}

/** Ask for a name before stashing. */
export function StashDialog({ path, repoName, git, onClose, onDone }: { path: string; repoName: string; git: GitStatus; onClose: () => void; onDone: (msg: string) => void }) {
  const [name, setName] = useState("");
  const [untracked, setUntracked] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const tracked = git.changes.length;
  const nUntracked = git.untracked.length;

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const out = await api.gitStash(path, name, untracked);
      onDone(`${repoName}: ${name.trim() ? `stashed as “${name.trim()}”` : out}`);
    } catch (e) {
      setError(errText(e));
      setBusy(false);
    }
  };

  return (
    <Modal
      title={`Stash changes in ${repoName}`}
      icon="inventory"
      onClose={busy ? () => undefined : onClose}
      footer={
        <>
          <Btn variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Btn>
          <Btn variant="primary" icon={busy ? "progress_activity" : "inventory"} disabled={busy || (tracked === 0 && (!untracked || nUntracked === 0))} onClick={() => void submit()}>
            {busy ? "Stashing…" : "Stash"}
          </Btn>
        </>
      }
    >
      <Field label="Stash name" hint="So you can recognise it later, e.g. “half-done login form”. Optional.">
        <input
          autoFocus
          className={inputCls}
          value={name}
          maxLength={120}
          placeholder={`WIP on ${git.branch}`}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void submit()}
        />
      </Field>
      <label className="mt-space-md flex items-center gap-2 text-[12px] text-on-surface-variant">
        <input type="checkbox" checked={untracked} onChange={(e) => setUntracked(e.target.checked)} />
        Include untracked files ({nUntracked})
      </label>
      <div className="mt-1 text-[11px] text-outline">
        {tracked} modified file{tracked === 1 ? "" : "s"} on <span className="font-code-dense">{git.branch}</span> will be saved and the working tree reset.
      </div>
      {error && <ErrorBox text={error} />}
    </Modal>
  );
}

function BranchRow({ b, selected, onPick, remote }: { b: BranchInfo; selected: boolean; onPick: () => void; remote?: boolean }) {
  return (
    <button
      type="button"
      onClick={onPick}
      className={`w-full text-left flex items-center gap-2 px-2 py-1.5 rounded border ${selected ? "border-primary bg-primary/10" : "border-transparent hover:bg-surface-container-high"}`}
    >
      <Icon name={b.current ? "radio_button_checked" : remote ? "cloud" : "fork_right"} className={`text-[16px] ${b.current ? "text-secondary" : "text-outline"}`} />
      <span className="min-w-0 flex-1">
        <span className="block font-code-dense text-code-dense text-on-surface truncate">{b.name}</span>
        <span className="block text-[10px] text-outline truncate">{b.lastCommit}</span>
      </span>
      {b.current && <span className="text-[10px] text-secondary">current</span>}
      {b.upstream && !b.current && !remote && <span className="text-[10px] text-outline font-code-dense">→ {b.upstream}</span>}
    </button>
  );
}

/** Pick a branch (local, or remote-only) to switch to, or create a new one. */
export function BranchDialog({ path, repoName, git, onClose, onDone }: { path: string; repoName: string; git: GitStatus; onClose: () => void; onDone: (msg: string) => void }) {
  const [data, setData] = useState<Branches | null>(null);
  const [q, setQ] = useState("");
  const [pick, setPick] = useState<{ name: string; create: boolean } | null>(null);
  const [stash, setStash] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty = git.changes.length + git.untracked.length > 0;

  useEffect(() => {
    let stop = false;
    api.gitBranches(path).then((b) => !stop && setData(b)).catch((e) => !stop && setError(errText(e)));
    return () => {
      stop = true;
    };
  }, [path]);

  const needle = q.trim().toLowerCase();
  const local = useMemo(() => (data?.local ?? []).filter((b) => !needle || b.name.toLowerCase().includes(needle)), [data, needle]);
  const remote = useMemo(() => (data?.remote ?? []).filter((b) => !needle || b.name.toLowerCase().includes(needle)), [data, needle]);
  const exact = !!data && [...data.local, ...data.remote].some((b) => b.name === q.trim() || b.name.endsWith(`/${q.trim()}`));
  const canCreate = q.trim().length > 0 && !/\s/.test(q.trim()) && !q.trim().startsWith("-") && !exact;

  const go = async () => {
    if (!pick || busy) return;
    setBusy(true);
    setError("");
    try {
      const out = await api.gitCheckout(path, pick.name, pick.create, stash);
      onDone(`${repoName}: ${out}`);
    } catch (e) {
      setError(errText(e));
      setBusy(false);
    }
  };

  const isCurrent = !!pick && !pick.create && pick.name === data?.current;

  return (
    <Modal
      title={`Switch branch in ${repoName}`}
      icon="fork_right"
      width={520}
      onClose={busy ? () => undefined : onClose}
      footer={
        <>
          <Btn variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Btn>
          <Btn variant="primary" icon={busy ? "progress_activity" : pick?.create ? "add" : "swap_horiz"} disabled={!pick || busy || isCurrent} onClick={() => void go()}>
            {busy ? "Switching…" : pick?.create ? `Create & switch to ${pick.name}` : pick ? `Switch to ${pick.name}` : "Switch"}
          </Btn>
        </>
      }
    >
      <input
        autoFocus
        className={inputCls}
        value={q}
        placeholder="Filter branches, or type a new branch name…"
        onChange={(e) => {
          setQ(e.target.value);
          setPick(null);
        }}
        onKeyDown={(e) => e.key === "Enter" && (pick ? void go() : canCreate && setPick({ name: q.trim(), create: true }))}
      />
      <div className="mt-space-sm max-h-[320px] overflow-auto space-y-0.5">
        {!data && !error && <div className="text-[12px] text-outline p-2">Loading branches…</div>}
        {canCreate && (
          <button
            type="button"
            onClick={() => setPick({ name: q.trim(), create: true })}
            className={`w-full text-left flex items-center gap-2 px-2 py-1.5 rounded border ${pick?.create ? "border-primary bg-primary/10" : "border-transparent hover:bg-surface-container-high"}`}
          >
            <Icon name="add" className="text-[16px] text-primary" />
            <span className="text-body-sm text-on-surface">
              Create new branch <span className="font-code-dense">{q.trim()}</span> from <span className="font-code-dense">{data?.current ?? git.branch}</span>
            </span>
          </button>
        )}
        {local.length > 0 && <div className="px-2 pt-1 text-[10px] uppercase tracking-wider text-outline font-label-sm">Local</div>}
        {local.map((b) => (
          <BranchRow key={b.name} b={b} selected={pick?.name === b.name && !pick.create} onPick={() => setPick({ name: b.name, create: false })} />
        ))}
        {remote.length > 0 && <div className="px-2 pt-2 text-[10px] uppercase tracking-wider text-outline font-label-sm">On the remote only (creates a tracking branch)</div>}
        {remote.map((b) => (
          <BranchRow key={b.name} b={b} remote selected={pick?.name === b.name && !pick.create} onPick={() => setPick({ name: b.name, create: false })} />
        ))}
        {data && local.length === 0 && remote.length === 0 && !canCreate && <div className="text-[12px] text-outline p-2">No branches match.</div>}
      </div>
      {dirty && (
        <label className="mt-space-md flex items-start gap-2 text-[12px] text-on-surface-variant">
          <input type="checkbox" className="mt-0.5" checked={stash} onChange={(e) => setStash(e.target.checked)} />
          <span>
            Stash my uncommitted changes first ({git.changes.length + git.untracked.length}).
            <span className="block text-[11px] text-outline">Without this git carries them to the new branch, and refuses if they would be overwritten.</span>
          </span>
        </label>
      )}
      {error && <ErrorBox text={error} />}
    </Modal>
  );
}
