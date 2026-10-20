import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { Btn, Card, EmptyState, Icon, PageBody, PageHeader, inputCls } from "../components/ui";
import { PROFILES, type EnvVar, type Workspace } from "../lib/types";
import { useApp } from "../state/AppState";

const GLOBAL = "__global__";

function parseDotenv(text: string): EnvVar[] {
  const out: EnvVar[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const m = line.replace(/^export\s+/, "").match(/^([A-Za-z_][A-Za-z0-9_.]*)\s*=\s*(.*)$/);
    if (!m) continue;
    let v = m[2].trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1);
    out.push({ key: m[1], value: v, secret: /PASSWORD|SECRET|TOKEN|KEY|PRIVATE/i.test(m[1]), profile: null });
  }
  return out;
}

export default function EnvPortsPage() {
  const { active, saveWorkspace, toast } = useApp();
  const navigate = useNavigate();
  const [draft, setDraft] = useState<Workspace | null>(null);
  const [scope, setScope] = useState(GLOBAL);
  const [reveal, setReveal] = useState<Record<string, boolean>>({});
  const [showImport, setShowImport] = useState(false);
  const [importText, setImportText] = useState("");
  const [query, setQuery] = useState("");
  const [resolved, setResolved] = useState(false);

  // reload when the workspace id changes or after a save
  const serialized = active ? JSON.stringify(active) : "";
  useEffect(() => {
    setDraft(active ? structuredClone(active) : null);
  }, [serialized]); // eslint-disable-line react-hooks/exhaustive-deps

  const dirty = !!draft && !!active && JSON.stringify(draft) !== JSON.stringify(active);

  const vars: EnvVar[] = useMemo(() => {
    if (!draft) return [];
    return scope === GLOBAL ? draft.env : (draft.services.find((s) => s.name === scope)?.env ?? []);
  }, [draft, scope]);

  if (!active || !draft) {
    return (
      <PageBody>
        <EmptyState icon="vpn_key" title="No active workspace" action={<Btn variant="primary" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          Environment variables are configured per workspace, per service and per profile.
        </EmptyState>
      </PageBody>
    );
  }

  const setVars = (next: EnvVar[]) =>
    setDraft((d) => {
      if (!d) return d;
      if (scope === GLOBAL) return { ...d, env: next };
      return { ...d, services: d.services.map((s) => (s.name === scope ? { ...s, env: next } : s)) };
    });
  const update = (i: number, patch: Partial<EnvVar>) => setVars(vars.map((v, idx) => (idx === i ? { ...v, ...patch } : v)));

  const dupKeys = new Set(
    vars.filter((v, i) => vars.findIndex((o) => o.key === v.key && (o.profile ?? "") === (v.profile ?? "")) !== i).map((v) => v.key),
  );

  const effective = (): [string, string, string][] => {
    if (!draft || scope === GLOBAL) return [];
    const svc = draft.services.find((s) => s.name === scope);
    if (!svc) return [];
    const m = new Map<string, [string, string]>();
    for (const v of [...draft.env.map((v) => ({ ...v, from: "global" })), ...svc.env.map((v) => ({ ...v, from: scope }))]) {
      if (v.key && (!v.profile || v.profile === draft.profile)) m.set(v.key, [v.secret ? "••••••••" : v.value, v.from]);
    }
    if (svc.port && !m.has("PORT")) m.set("PORT", [String(svc.port), "auto"]);
    return [...m.entries()].map(([k, [v, f]]) => [k, v, f]);
  };

  const rows = vars.map((v, i) => ({ v, i })).filter(({ v }) => !query.trim() || v.key.toLowerCase().includes(query.toLowerCase()) || v.value.toLowerCase().includes(query.toLowerCase()));

  const doImport = () => {
    const parsed = parseDotenv(importText);
    if (parsed.length === 0) {
      toast("No KEY=VALUE lines found", "error");
      return;
    }
    const merged = [...vars];
    let added = 0;
    let updated = 0;
    parsed.forEach((p) => {
      const idx = merged.findIndex((v) => v.key === p.key && (v.profile ?? "") === "");
      if (idx >= 0) {
        merged[idx] = { ...merged[idx], value: p.value };
        updated++;
      } else {
        merged.push(p);
        added++;
      }
    });
    setVars(merged);
    setImportText("");
    setShowImport(false);
    toast(`Imported ${added} new, ${updated} updated`, "success");
  };

  return (
    <PageBody>
      <PageHeader
        title="Environment"
        subtitle={
          <>
            Variables are injected when a service starts. Active profile: <span className="font-code-dense text-primary">{active.profile}</span>
          </>
        }
        actions={
          <>
            <Btn variant="ghost" disabled={!dirty} onClick={() => setDraft(structuredClone(active))}>Discard</Btn>
            <Btn variant="primary" icon="save" disabled={!dirty} onClick={async () => { if (draft && (await saveWorkspace(draft))) toast("Environment saved: restart services to apply", "success"); }}>
              Save changes
            </Btn>
          </>
        }
      />

      <div className="grid gap-space-md" style={{ gridTemplateColumns: "240px minmax(0,1fr)" }}>
        <Card title="Scope" icon="layers" bodyClass="p-1">
          {[{ id: GLOBAL, label: "Global (all services)", count: draft.env.length }, ...draft.services.map((s) => ({ id: s.name, label: s.name, count: s.env.length }))].map((s) => (
            <button key={s.id} type="button" onClick={() => setScope(s.id)} className={`w-full flex items-center justify-between px-2 py-1.5 rounded text-left ${scope === s.id ? "bg-surface-container-high text-primary" : "text-on-surface-variant hover:bg-surface-container-high hover:text-on-surface"}`}>
              <span className="font-label-md text-label-md truncate">{s.label}</span>
              <span className="text-[10px] text-outline">{s.count}</span>
            </button>
          ))}
        </Card>

        <Card
          title={scope === GLOBAL ? "Global variables" : `${scope} variables`}
          icon="vpn_key"
          bodyClass="p-0"
          actions={
            <div className="flex items-center gap-2">
              {scope !== GLOBAL && (
                <button type="button" onClick={() => setResolved((r) => !r)} className={`text-[11px] ${resolved ? "text-primary" : "text-on-surface-variant hover:text-on-surface"}`}>
                  {resolved ? "Editing view" : "Show resolved"}
                </button>
              )}
              <Btn icon="upload_file" className="!px-2 !py-1" onClick={() => setShowImport((s) => !s)}>Import .env</Btn>
              <Btn icon="add" variant="primary" className="!px-2 !py-1" onClick={() => setVars([...vars, { key: "", value: "", secret: false, profile: null }])}>Add variable</Btn>
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
                placeholder={"# paste the contents of a .env file\nDATABASE_URL=postgres://postgres:postgres@localhost:5432/app\nSTRIPE_SECRET_KEY=sk_test_123"}
                className="w-full p-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg font-code-dense text-code-dense text-on-surface focus:outline-none focus:border-primary select-text"
              />
              <div className="flex gap-2 mt-2">
                <Btn variant="primary" onClick={doImport}>Import</Btn>
                <Btn variant="ghost" onClick={() => setShowImport(false)}>Cancel</Btn>
              </div>
            </div>
          )}

          {resolved && scope !== GLOBAL ? (
            <table className="w-full font-code-dense text-code-dense">
              <thead><tr className="text-[10px] uppercase text-outline text-left border-b border-surface-container-highest"><th className="px-space-md py-2">Key</th><th className="px-space-md py-2">Value</th><th className="px-space-md py-2">From</th></tr></thead>
              <tbody>
                {effective().map(([k, v, f]) => (
                  <tr key={k} className="border-b border-surface-container-highest last:border-0">
                    <td className="px-space-md py-1.5 text-primary">{k}</td>
                    <td className="px-space-md py-1.5 text-on-surface select-text break-all">{v}</td>
                    <td className="px-space-md py-1.5 text-outline">{f}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <>
              {vars.length > 6 && (
                <div className="p-space-sm border-b border-surface-container-highest">
                  <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Filter variables…" className={inputCls} />
                </div>
              )}
              {vars.length === 0 ? (
                <div className="px-space-md py-10 text-center text-on-surface-variant text-body-sm">
                  <Icon name="vpn_key" className="text-[32px] text-outline block mx-auto mb-2" />
                  No variables yet. {scope !== GLOBAL && <>Global variables still apply to this service. </>}Add one or import a <code>.env</code> file.
                </div>
              ) : (
                <table className="w-full">
                  <thead>
                    <tr className="text-[10px] font-label-sm uppercase tracking-wider text-outline text-left border-b border-surface-container-highest">
                      <th className="px-space-md py-2 w-1/4">Key</th>
                      <th className="px-space-md py-2">Value</th>
                      <th className="px-space-md py-2 w-36">Profile</th>
                      <th className="px-space-md py-2 w-16 text-center">Secret</th>
                      <th className="w-10" />
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map(({ v, i }) => (
                      <tr key={i} className="border-b border-surface-container-highest last:border-0">
                        <td className="px-space-md py-1.5">
                          <input value={v.key} onChange={(e) => update(i, { key: e.target.value.replace(/\s/g, "_") })} placeholder="KEY" className={`${inputCls} font-code-dense ${dupKeys.has(v.key) ? "!border-error" : ""}`} title={dupKeys.has(v.key) ? "Duplicate key for this profile" : undefined} />
                        </td>
                        <td className="px-space-md py-1.5">
                          <div className="flex items-center gap-1">
                            <input type={v.secret && !reveal[`${scope}${i}`] ? "password" : "text"} value={v.value} onChange={(e) => update(i, { value: e.target.value })} placeholder="value" className={`${inputCls} font-code-dense`} />
                            {v.secret && (
                              <button type="button" onClick={() => setReveal((r) => ({ ...r, [`${scope}${i}`]: !r[`${scope}${i}`] }))} className="text-outline hover:text-on-surface">
                                <Icon name={reveal[`${scope}${i}`] ? "visibility_off" : "visibility"} className="text-[16px]" />
                              </button>
                            )}
                          </div>
                        </td>
                        <td className="px-space-md py-1.5">
                          <select value={v.profile ?? ""} onChange={(e) => update(i, { profile: e.target.value || null })} className={`${inputCls} !px-1`}>
                            <option value="">All profiles</option>
                            {PROFILES.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
                          </select>
                        </td>
                        <td className="px-space-md py-1.5 text-center">
                          <input type="checkbox" className="accent-primary" checked={v.secret} onChange={(e) => update(i, { secret: e.target.checked })} />
                        </td>
                        <td className="px-1">
                          <button type="button" title="Delete" onClick={() => setVars(vars.filter((_, idx) => idx !== i))} className="text-outline hover:text-error">
                            <Icon name="delete" className="text-[18px]" />
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </>
          )}
        </Card>
      </div>
      {dirty && <p className="text-[11px] text-tertiary mt-2">Unsaved changes. Running services keep their old environment until restarted.</p>}
    </PageBody>
  );
}
