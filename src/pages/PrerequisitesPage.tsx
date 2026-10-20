import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Btn, Card, EmptyState, Icon, PageBody, PageHeader, inputCls } from "../components/ui";
import { api } from "../lib/tauri";
import type { Prerequisite, PrerequisiteResult, Workspace } from "../lib/types";
import { useApp } from "../state/AppState";

type Row = { state: "running" } | { state: "done"; result: PrerequisiteResult };

const EXAMPLES: Prerequisite[] = [
  { label: "Java 8", command: "java -version", expected: "1.8" },
  { label: "Node 24", command: "node -v", expected: "v24" },
];

const FORMAT_HINT = "# one per line:  label = command => must contain (optional)\nJava 8 = java -version => 1.8\nNode 24 = node -v => v24\nGit = git --version";

/** `Label = command => expected` per line. Also accepts tab-separated columns (paste from a sheet),
 *  markdown bullets and `backticks`. `#` and `//` lines are comments. */
function parsePrerequisites(text: string): { items: Prerequisite[]; skipped: number } {
  const items: Prerequisite[] = [];
  let skipped = 0;
  for (const raw of text.split(/\r?\n/)) {
    let line = raw.trim().replace(/^([-*+]|\d+[.)])\s+/, "");
    if (!line || line.startsWith("#") || line.startsWith("//")) continue;
    let label = "";
    let rest = "";
    if (line.includes("\t")) {
      const [a, b, c] = line.split("\t").map((s) => s.trim());
      label = a ?? "";
      rest = c ? `${b} => ${c}` : (b ?? "");
    } else {
      const eq = line.indexOf("=");
      if (eq <= 0) {
        skipped++;
        continue;
      }
      label = line.slice(0, eq).trim();
      rest = line.slice(eq + 1).trim();
    }
    const arrow = rest.lastIndexOf("=>");
    const command = (arrow >= 0 ? rest.slice(0, arrow) : rest).trim().replace(/^`(.*)`$/, "$1").trim();
    const expected = (arrow >= 0 ? rest.slice(arrow + 2) : "").trim().replace(/^["'`](.*)["'`]$/, "$1");
    if (!label || !command) {
      skipped++;
      continue;
    }
    items.push({ label, command, expected });
  }
  return { items, skipped };
}

const toText = (items: Prerequisite[]) =>
  items.filter((p) => p.label.trim() || p.command.trim()).map((p) => `${p.label.trim()} = ${p.command.trim()}${p.expected?.trim() ? ` => ${p.expected.trim()}` : ""}`).join("\n");

export default function PrerequisitesPage() {
  const { active, saveWorkspace, toast } = useApp();
  const navigate = useNavigate();
  const [showImport, setShowImport] = useState(false);
  const [importText, setImportText] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState<Workspace | null>(null);
  const [rows, setRows] = useState<Record<number, Row>>({});
  const [checking, setChecking] = useState(false);
  const stop = useRef(false);

  const serialized = active ? JSON.stringify(active) : "";
  useEffect(() => {
    setDraft(active ? structuredClone(active) : null);
    setRows({});
  }, [active?.id]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    // after a save keep results, but pick up the persisted copy
    setDraft(active ? structuredClone(active) : null);
  }, [serialized]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => () => { stop.current = true; }, []);

  const dirty = !!draft && !!active && JSON.stringify(draft) !== JSON.stringify(active);

  if (!active || !draft) {
    return (
      <PageBody>
        <EmptyState icon="checklist" title="No active workspace" action={<Btn variant="primary" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          Prerequisites are the tools a workspace needs on your machine, such as Java 8 or Node 24.
        </EmptyState>
      </PageBody>
    );
  }

  const items = draft.prerequisites ?? [];
  const setItems = (next: Prerequisite[]) => setDraft((d) => (d ? { ...d, prerequisites: next } : d));
  const update = (i: number, patch: Partial<Prerequisite>) => {
    setItems(items.map((p, idx) => (idx === i ? { ...p, ...patch } : p)));
    setRows((r) => {
      const { [i]: _drop, ...rest } = r;
      return rest;
    });
  };
  const remove = (i: number) => {
    setItems(items.filter((_, idx) => idx !== i));
    setRows({});
  };

  const runnable = items.map((p, i) => ({ p, i })).filter(({ p }) => p.command.trim());
  const results = Object.values(rows).filter((r): r is { state: "done"; result: PrerequisiteResult } => r.state === "done");
  const passed = results.filter((r) => r.result.ok).length;
  const failed = results.length - passed;

  /** Add the parsed lines; a line with an existing label replaces that row. */
  const importFrom = (text: string): boolean => {
    const { items: parsed, skipped } = parsePrerequisites(text);
    if (parsed.length === 0) {
      toast(skipped ? `No valid lines found (${skipped} skipped). Use: label = command` : "Nothing to import", "error");
      return false;
    }
    const merged = [...items.filter((p) => p.label.trim() || p.command.trim())];
    let added = 0;
    let updated = 0;
    for (const p of parsed) {
      const idx = merged.findIndex((m) => m.label.trim().toLowerCase() === p.label.toLowerCase());
      if (idx >= 0) {
        merged[idx] = p;
        updated++;
      } else {
        merged.push(p);
        added++;
      }
    }
    setItems(merged);
    setRows({});
    toast(`Imported ${added} new${updated ? `, ${updated} updated` : ""}${skipped ? `, ${skipped} skipped` : ""}. Save to keep them.`, "success");
    return true;
  };

  const onFile = async (file?: File) => {
    if (!file) return;
    importFrom(await file.text());
  };

  const copyAll = async () => {
    try {
      await navigator.clipboard.writeText(toText(items));
      toast("Copied as text. Paste it into your docs or another workspace.", "success");
    } catch {
      toast("Could not copy to the clipboard", "error");
    }
  };

  const check = async () => {
    stop.current = false;
    setChecking(true);
    setRows({});
    for (const { p, i } of runnable) {
      if (stop.current) break;
      setRows((r) => ({ ...r, [i]: { state: "running" } }));
      try {
        const result = await api.checkPrerequisite({ label: p.label.trim() || p.command.trim(), command: p.command, expected: p.expected ?? "" });
        setRows((r) => ({ ...r, [i]: { state: "done", result } }));
      } catch (e) {
        setRows((r) => ({
          ...r,
          [i]: { state: "done", result: { label: p.label, command: p.command, expected: p.expected, ok: false, code: null, output: "", error: String(e), durationMs: 0 } },
        }));
      }
    }
    setChecking(false);
  };

  const save = async () => {
    const cleaned = items.filter((p) => p.label.trim() || p.command.trim()).map((p) => ({ label: p.label.trim(), command: p.command.trim(), expected: (p.expected ?? "").trim() }));
    if (await saveWorkspace({ ...draft, prerequisites: cleaned })) toast("Prerequisites saved", "success");
  };

  return (
    <PageBody>
      <PageHeader
        title="Prerequisites"
        subtitle={
          <>
            Tools <span className="text-on-surface">{active.name}</span> needs on this machine. A check passes when the command exits with code 0 and, if set, its output contains the expected text (such as a version).
          </>
        }
        actions={
          <>
            <Btn variant="ghost" disabled={!dirty} onClick={() => { setDraft(structuredClone(active)); setRows({}); }}>Discard</Btn>
            <Btn icon="save" disabled={!dirty} onClick={save}>Save changes</Btn>
            {checking ? (
              <Btn icon="stop" onClick={() => { stop.current = true; }}>Stop</Btn>
            ) : (
              <Btn variant="primary" icon="play_arrow" disabled={runnable.length === 0} onClick={check}>Check prerequisites</Btn>
            )}
          </>
        }
      />

      <Card
        title="Required tools"
        icon="checklist"
        bodyClass="p-0"
        actions={
          <div className="flex items-center gap-3">
            {results.length > 0 && (
              <span className="text-[11px]">
                <span className="text-secondary">{passed} passed</span>
                {failed > 0 && <span className="text-error">, {failed} failed</span>}
              </span>
            )}
            <input ref={fileInput} type="file" accept=".txt,.md,.list,.prereq,text/plain" className="hidden" onChange={(e) => { void onFile(e.target.files?.[0]); e.target.value = ""; }} />
            {items.length > 0 && <Btn icon="content_copy" className="!px-2 !py-1" onClick={copyAll}>Copy as text</Btn>}
            <Btn icon="upload_file" className="!px-2 !py-1" onClick={() => fileInput.current?.click()}>Import file</Btn>
            <Btn icon="playlist_add" className="!px-2 !py-1" onClick={() => setShowImport((s) => !s)}>Paste list</Btn>
            <Btn icon="add" variant="primary" className="!px-2 !py-1" onClick={() => setItems([...items, { label: "", command: "", expected: "" }])}>Add prerequisite</Btn>
          </div>
        }
      >
        {showImport && (
          <div className="p-space-md border-b border-surface-container-highest bg-surface-container-low">
            <textarea
              autoFocus
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              rows={6}
              placeholder={FORMAT_HINT}
              className="w-full p-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg font-code-dense text-code-dense text-on-surface focus:outline-none focus:border-primary select-text"
            />
            <p className="text-[11px] text-outline mt-1">One per line: <code>label = command =&gt; must contain</code>. Lines with an existing label replace that row. Bullets, backticks and tab-separated columns work too.</p>
            <div className="flex gap-2 mt-2">
              <Btn variant="primary" onClick={() => { if (importFrom(importText)) { setImportText(""); setShowImport(false); } }}>Import</Btn>
              <Btn variant="ghost" onClick={() => setShowImport(false)}>Cancel</Btn>
            </div>
          </div>
        )}
        {items.length === 0 ? (
          <div className="px-space-md py-10 text-center text-on-surface-variant text-body-sm">
            <Icon name="checklist" className="text-[32px] text-outline block mx-auto mb-2" />
            <p>No prerequisites yet. Add a label and a command that prints or checks a version.</p>
            <div className="flex justify-center gap-2 mt-3">
              <Btn icon="upload_file" onClick={() => fileInput.current?.click()}>Import file</Btn>
              <Btn icon="playlist_add" onClick={() => setShowImport(true)}>Paste list</Btn>
              <Btn icon="auto_awesome" onClick={() => setItems(EXAMPLES)}>Add Java and Node examples</Btn>
            </div>
          </div>
        ) : (
          <ul>
            {items.map((p, i) => {
              const row = rows[i];
              return (
                <li key={i} className="border-b border-surface-container-highest last:border-0 px-space-md py-2">
                  <div className="flex items-center gap-2">
                    <StatusIcon row={row} />
                    <div className="w-40 shrink-0">
                      <input value={p.label} onChange={(e) => update(i, { label: e.target.value })} placeholder="Label (Java 8)" className={inputCls} />
                    </div>
                    <div className="flex-1 min-w-0">
                      <input value={p.command} onChange={(e) => update(i, { command: e.target.value })} placeholder="Command (java -version)" className={`${inputCls} font-code-dense`} />
                    </div>
                    <div className="w-36 shrink-0">
                      <input value={p.expected ?? ""} onChange={(e) => update(i, { expected: e.target.value })} placeholder="Must contain (v24)" title="Optional: the output must contain this text, e.g. a version" className={`${inputCls} font-code-dense`} />
                    </div>
                    <ResultInfo row={row} />
                    <button type="button" title="Remove" onClick={() => remove(i)} className="text-outline hover:text-error shrink-0">
                      <Icon name="delete" className="text-[18px]" />
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </Card>
      {dirty && <p className="text-[11px] text-tertiary mt-2">Unsaved changes. You can still run a check before saving.</p>}
    </PageBody>
  );
}

/** Info icon that opens the result (status line + output) in a popover, so rows stay one line tall. */
function ResultInfo({ row }: { row?: Row }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  // keeps the row layout stable before there is a result
  if (row?.state !== "done") return <span className="w-[18px] shrink-0" />;
  const r = row.result;
  const summary = r.ok ? (r.expected ? `Found, output contains "${r.expected}"` : "Found") : r.error ?? "Failed";
  const output = r.output.length > 1500 ? `${r.output.slice(0, 1500)}…` : r.output;
  return (
    <div ref={box} className="relative shrink-0">
      <button type="button" title={summary} onClick={() => setOpen((o) => !o)} className={`flex ${r.ok ? "text-outline hover:text-on-surface" : "text-error"}`}>
        <Icon name="info" className="text-[18px]" />
      </button>
      {open && (
        <div className="absolute right-0 top-full mt-1 z-30 w-[28rem] max-w-[70vw] p-3 rounded-lg border border-surface-container-highest bg-surface-container-high shadow-xl">
          <p className={`text-[12px] font-medium ${r.ok ? "text-secondary" : "text-error"}`}>
            {summary}
            <span className="text-outline font-normal"> · {(r.durationMs / 1000).toFixed(1)}s</span>
          </p>
          <p className="font-code-dense text-code-dense text-outline mt-1 break-all">$ {r.command}</p>
          {output ? (
            <pre className="mt-2 p-2 rounded bg-surface-container-lowest font-code-dense text-code-dense text-on-surface-variant whitespace-pre-wrap break-all select-text max-h-48 overflow-auto">{output}</pre>
          ) : (
            <p className="text-[11px] text-outline mt-2">No output.</p>
          )}
        </div>
      )}
    </div>
  );
}

function StatusIcon({ row }: { row?: Row }) {
  if (!row) return <Icon name="radio_button_unchecked" className="text-[20px] text-outline shrink-0" />;
  if (row.state === "running") return <Icon name="progress_activity" className="text-[20px] text-primary shrink-0 animate-spin" />;
  return row.result.ok ? (
    <Icon name="check_circle" className="text-[20px] text-secondary shrink-0" />
  ) : (
    <Icon name="cancel" className="text-[20px] text-error shrink-0" />
  );
}
