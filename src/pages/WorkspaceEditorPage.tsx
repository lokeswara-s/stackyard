import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";

import { Btn, Card, ConfirmBtn, Field, Icon, PageBody, PageHeader, inputCls } from "../components/ui";
import { PLUGINS, pluginToService } from "../lib/catalog";
import { api, pickFile, pickFolder } from "../lib/tauri";
import type { AppliedCommand, Service, ShellInfo, Workspace } from "../lib/types";
import { useApp } from "../state/AppState";

const blank = (): Workspace => ({ id: "", name: "", description: "", profile: "local", syncBeforeUp: true, services: [], env: [] });

function uniqueName(base: string, taken: string[]) {
  const clean = base.trim().replace(/[^a-zA-Z0-9._-]+/g, "-") || "service";
  let name = clean;
  let n = 2;
  while (taken.includes(name)) name = `${clean}-${n++}`;
  return name;
}

/** Shows what will really run, and how the port reaches the service (flag, env var, or auto-detected). */
function CommandPreview({ repoPath, command, port }: { repoPath: string; command: string; port: number | null }) {
  const [applied, setApplied] = useState<AppliedCommand | null>(null);
  useEffect(() => {
    if (!command.trim()) {
      setApplied(null);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      api.previewCommand(repoPath, command, port).then((a) => live && setApplied(a)).catch(() => live && setApplied(null));
    }, 250);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [repoPath, command, port]);

  if (!command.trim()) return null;
  return (
    <div className="text-[11px] text-on-surface-variant leading-snug">
      {port == null ? (
        <span>
          No port set: the app picks its own and Stackyard <b className="text-on-surface">detects it once running</b>.
        </span>
      ) : (
        <>
          <div>
            Runs: <code className="font-code-dense text-on-surface break-all">{applied?.command ?? command}</code>
          </div>
          {applied?.notes.map((n, i) => (
            <div key={i} className="text-outline">{n}</div>
          ))}
        </>
      )}
    </div>
  );
}

export default function WorkspaceEditorPage() {
  const { id } = useParams();
  const { store, saveWorkspace, deleteWorkspace, setActive, toast, ready } = useApp();
  const navigate = useNavigate();
  const [draft, setDraft] = useState<Workspace | null>(null);
  const [busy, setBusy] = useState(false);
  const [shells, setShells] = useState<ShellInfo[]>([]);

  useEffect(() => {
    api.shellOptions().then(setShells).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (!ready) return;
    if (id) {
      const found = store.workspaces.find((w) => w.id === id);
      setDraft(found ? structuredClone(found) : null);
    } else {
      setDraft(blank());
    }
    // only (re)initialise when the route changes, not on every store refresh
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, ready]);

  if (!ready) return null;
  if (!draft) {
    return (
      <PageBody>
        <p className="text-on-surface-variant">Workspace not found.</p>
      </PageBody>
    );
  }

  const set = (patch: Partial<Workspace>) => setDraft((d) => (d ? { ...d, ...patch } : d));
  const names = draft.services.map((s) => s.name);

  const updateService = (idx: number, patch: Partial<Service>) =>
    setDraft((d) => {
      if (!d) return d;
      const services = d.services.map((s, i) => (i === idx ? { ...s, ...patch } : s));
      // keep dependency references valid when a service is renamed
      if (patch.name !== undefined && patch.name !== d.services[idx].name) {
        const old = d.services[idx].name;
        services.forEach((s) => (s.dependsOn = s.dependsOn.map((x) => (x === old ? patch.name! : x))));
      }
      return { ...d, services };
    });

  const removeService = (idx: number) =>
    setDraft((d) => {
      if (!d) return d;
      const gone = d.services[idx].name;
      return { ...d, services: d.services.filter((_, i) => i !== idx).map((s) => ({ ...s, dependsOn: s.dependsOn.filter((x) => x !== gone) })) };
    });

  const addRepo = async () => {
    const folder = await pickFolder("Select a repository / project folder");
    if (!folder) return;
    try {
      const d = await api.detectProject(folder);
      const svc: Service = {
        name: uniqueName(d.name, names),
        kind: "app",
        stack: d.stack,
        repoPath: folder,
        command: d.command,
        port: d.port ?? null,
        dependsOn: [],
        env: [],
      };
      setDraft((cur) => (cur ? { ...cur, services: [...cur.services, svc] } : cur));
      toast(`Added ${svc.name} (${d.stack})${d.notes.length ? `: ${d.notes[0]}` : ""}`, d.notes.length ? "info" : "success");
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  };

  const addBlankService = () =>
    setDraft((d) =>
      d ? { ...d, services: [...d.services, { name: uniqueName("service", d.services.map((s) => s.name)), kind: "app", stack: "", repoPath: "", command: "", port: null, dependsOn: [], env: [] }] } : d,
    );

  const problems: string[] = [];
  if (!draft.name.trim()) problems.push("Give the workspace a name");
  const ports = draft.services.filter((s) => s.port).map((s) => s.port as number);
  const dupPort = ports.find((p, i) => ports.indexOf(p) !== i);
  if (dupPort) problems.push(`Port ${dupPort} is used by more than one service`);
  {
    // circular "starts after" links make Up impossible: catch them while editing
    const deps = new Map(draft.services.map((s) => [s.name, s.dependsOn]));
    const state = new Map<string, 1 | 2>();
    let cyclic: string | null = null;
    const visit = (n: string, trail: string[]) => {
      if (cyclic) return;
      if (state.get(n) === 2) return;
      if (state.get(n) === 1) {
        cyclic = [...trail.slice(trail.indexOf(n)), n].join(" → ");
        return;
      }
      state.set(n, 1);
      (deps.get(n) ?? []).forEach((d) => deps.has(d) && visit(d, [...trail, n]));
      state.set(n, 2);
    };
    draft.services.forEach((s) => visit(s.name, []));
    if (cyclic) problems.push(`Circular dependency: ${cyclic}`);
  }
  draft.services.forEach((s) => {
    if (s.kind === "app" && !s.repoPath.trim()) problems.push(`${s.name}: choose a folder`);
    if (s.kind === "app" && !s.command.trim()) problems.push(`${s.name}: start command is empty`);
    if (s.kind === "infra" && !s.image?.trim()) problems.push(`${s.name}: Docker image is empty`);
  });

  const save = async () => {
    setBusy(true);
    const saved = await saveWorkspace(draft);
    setBusy(false);
    if (!saved) return;
    await setActive(saved.id);
    toast(`Workspace “${saved.name}” saved`, "success");
    navigate("/");
  };

  const numInput = (v: number | null | undefined, on: (n: number | null) => void, ph: string) => (
    <input
      className={inputCls}
      inputMode="numeric"
      placeholder={ph}
      value={v ?? ""}
      onChange={(e) => {
        const t = e.target.value.replace(/\D/g, "").slice(0, 5);
        on(t ? Math.min(65535, Number(t)) : null);
      }}
    />
  );

  return (
    <PageBody>
      <PageHeader
        title={id ? `Edit ${draft.name || "workspace"}` : "New workspace"}
        subtitle="Connect repositories and infra. Ports, dependencies and start commands are used by the Up pipeline."
        actions={
          <>
            <Btn variant="ghost" onClick={() => navigate(-1)}>
              Cancel
            </Btn>
            <Btn variant="primary" icon="save" disabled={busy || problems.length > 0} onClick={() => void save()} title={problems[0]}>
              {id ? "Save changes" : "Create workspace"}
            </Btn>
          </>
        }
      />

      {problems.length > 0 && draft.services.length > 0 && (
        <div className="mb-space-md px-space-md py-space-sm rounded-lg border border-tertiary/30 bg-tertiary/10 text-body-sm text-on-surface">
          <b className="text-tertiary">To fix before saving:</b> {problems.join(" · ")}
        </div>
      )}

      <div className="grid gap-space-md" style={{ gridTemplateColumns: "minmax(0,1fr) 320px" }}>
        <div className="space-y-space-md min-w-0">
          <Card title="Services" icon="deployed_code" actions={
            <div className="flex gap-2">
              <Btn icon="folder_open" variant="primary" onClick={() => void addRepo()}>
                Add repository…
              </Btn>
              <Btn icon="add" onClick={addBlankService}>
                Blank service
              </Btn>
            </div>
          }>
            {draft.services.length === 0 ? (
              <div className="text-center py-8 text-on-surface-variant text-body-sm">
                <Icon name="folder_open" className="text-[32px] text-outline block mx-auto mb-2" />
                Add a repository folder: the start command and port are detected from <code>package.json</code>, <code>Cargo.toml</code>, <code>go.mod</code> and similar.
              </div>
            ) : (
              <div className="space-y-space-md">
                {draft.services.map((s, idx) => (
                  <div key={idx} className="rounded-lg border border-surface-container-highest bg-surface-container-low p-space-md">
                    <div className="flex items-center justify-between mb-space-sm">
                      <div className="flex items-center gap-2">
                        <Icon name={s.kind === "infra" ? "database" : "terminal"} className="text-[18px] text-primary" />
                        <span className="font-headline-sm text-headline-sm text-on-surface">{s.name}</span>
                        <span className="text-[10px] px-1.5 rounded bg-surface-container-highest text-on-surface-variant">{s.kind === "infra" ? "Docker" : s.stack || "app"}</span>
                      </div>
                      <ConfirmBtn icon="delete" confirmLabel="Remove?" onConfirm={() => removeService(idx)} className="!px-2 !py-1">
                        Remove
                      </ConfirmBtn>
                    </div>
                    <div className="grid grid-cols-6 gap-space-sm">
                      <div className="col-span-2">
                        <Field label="Name">
                          <input className={inputCls} value={s.name} onChange={(e) => updateService(idx, { name: e.target.value.replace(/[^a-zA-Z0-9._-]/g, "-") })} />
                        </Field>
                      </div>
                      {s.kind === "app" ? (
                        <div className="col-span-4">
                          <Field label="Folder">
                            <div className="flex gap-1">
                              <input className={inputCls} value={s.repoPath} onChange={(e) => updateService(idx, { repoPath: e.target.value })} placeholder="C:\code\my-service" />
                              <Btn
                                icon="folder_open"
                                className="!px-2"
                                onClick={async () => {
                                  const f = await pickFolder();
                                  if (f) updateService(idx, { repoPath: f });
                                }}
                              />
                            </div>
                          </Field>
                        </div>
                      ) : (
                        <>
                          <div className="col-span-3">
                            <Field label="Docker image">
                              <input className={inputCls} value={s.image ?? ""} onChange={(e) => updateService(idx, { image: e.target.value })} />
                            </Field>
                          </div>
                          <div className="col-span-1">
                            <Field label="Container port">{numInput(s.containerPort, (n) => updateService(idx, { containerPort: n }), "5432")}</Field>
                          </div>
                        </>
                      )}
                      <div className={s.kind === "app" ? "col-span-3" : "col-span-4"}>
                        <Field label={s.kind === "app" ? "Start command" : "Extra container args (optional)"}>
                          <input className={`${inputCls} font-code-dense`} value={s.command} onChange={(e) => updateService(idx, { command: e.target.value })} placeholder={s.kind === "app" ? "npm run dev" : ""} />
                        </Field>
                      </div>
                      {s.kind === "app" && (
                        <div className="col-span-1">
                          <Field label="Terminal" hint={shells.find((x) => x.id === (s.shell ?? ""))?.available === false ? "Not installed" : undefined}>
                            <select className={inputCls} value={s.shell ?? ""} onChange={(e) => updateService(idx, { shell: e.target.value })}>
                              {(shells.length ? shells : [{ id: "", label: "Default", available: true, path: null }]).map((o) => (
                                <option key={o.id} value={o.id} disabled={!o.available && o.id !== (s.shell ?? "")}>
                                  {o.label}
                                  {!o.available ? " (not installed)" : ""}
                                </option>
                              ))}
                            </select>
                          </Field>
                        </div>
                      )}
                      <div className="col-span-1">
                        <Field label={s.kind === "infra" ? "Host port" : "Port (optional)"}>
                          {numInput(s.port, (n) => updateService(idx, { port: n }), s.kind === "infra" ? "5432" : "auto")}
                        </Field>
                      </div>
                      {s.kind === "app" && (
                        <div className="col-span-5 -mt-1">
                          <CommandPreview repoPath={s.repoPath} command={s.command} port={s.port ?? null} />
                        </div>
                      )}
                      {s.kind === "app" && (
                        <div className="col-span-6 flex flex-wrap items-end gap-x-space-md gap-y-1.5 rounded-lg border border-surface-container-highest bg-surface-container-lowest/60 px-space-sm py-space-sm">
                          <label className="flex items-start gap-2 cursor-pointer min-w-[240px]">
                            <input type="checkbox" className="mt-0.5 accent-primary" checked={!s.noProfile} onChange={(e) => updateService(idx, { noProfile: !e.target.checked })} />
                            <span>
                              <span className="block font-label-md text-label-md text-on-surface">Load the terminal's profile</span>
                              <span className="block text-[10px] text-outline max-w-[300px] truncate" title={shells.find((x) => x.id === (s.shell ?? ""))?.profile ?? undefined}>
                                {shells.find((x) => x.id === (s.shell ?? ""))?.profile ?? "Your usual profile, aliases and custom commands"}
                              </span>
                            </span>
                          </label>
                          <div className="flex-1 min-w-[260px]">
                            <Field label="Init script (optional)" hint="Sourced in the same terminal right before the start command: custom functions, aliases, PATH.">
                              <div className="flex gap-1">
                                <input className={`${inputCls} font-code-dense`} value={s.shellInit ?? ""} onChange={(e) => updateService(idx, { shellInit: e.target.value })} placeholder="C:\Users\me\dev-tools.ps1" />
                                <Btn
                                  icon="description"
                                  className="!px-2"
                                  title="Choose a script"
                                  onClick={async () => {
                                    const f = await pickFile("Choose an init script", ["ps1", "cmd", "bat", "sh", "bash"]);
                                    if (f) updateService(idx, { shellInit: f });
                                  }}
                                />
                              </div>
                            </Field>
                          </div>
                          {draft.services.filter((o) => o.kind === "app").length > 1 && (
                            <Btn
                              variant="ghost"
                              icon="content_copy"
                              className="!px-2 !py-1"
                              title="Use this terminal, profile and init script for every app in the workspace"
                              onClick={() =>
                                setDraft((d) => (d ? { ...d, services: d.services.map((o) => (o.kind === "app" ? { ...o, shell: s.shell ?? "", noProfile: s.noProfile ?? false, shellInit: s.shellInit ?? "" } : o)) } : d))
                              }
                            >
                              Apply to all apps
                            </Btn>
                          )}
                        </div>
                      )}
                      {s.kind === "infra" && (
                        <div className="col-span-1">
                          <Field label="Data path">
                            <input className={inputCls} value={s.dataPath ?? ""} onChange={(e) => updateService(idx, { dataPath: e.target.value || null })} placeholder="/data" />
                          </Field>
                        </div>
                      )}
                    </div>
                    {draft.services.length > 1 && (
                      <div className="mt-space-sm">
                        <div className="font-label-sm text-label-sm text-on-surface-variant mb-1">Starts after</div>
                        <div className="flex flex-wrap gap-1.5">
                          {draft.services
                            .filter((o) => o.name !== s.name)
                            .map((o) => {
                              const on = s.dependsOn.includes(o.name);
                              return (
                                <button
                                  key={o.name}
                                  type="button"
                                  onClick={() => updateService(idx, { dependsOn: on ? s.dependsOn.filter((x) => x !== o.name) : [...s.dependsOn, o.name] })}
                                  className={`px-2 py-0.5 rounded-full border text-[11px] font-code-dense ${on ? "bg-primary/15 border-primary/50 text-primary" : "border-outline-variant text-on-surface-variant hover:text-on-surface"}`}
                                >
                                  {on && "✓ "}
                                  {o.name}
                                </button>
                              );
                            })}
                        </div>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </Card>
        </div>

        <div className="space-y-space-md">
          <Card title="Workspace" icon="tune">
            <div className="space-y-space-sm">
              <Field label="Name">
                <input className={inputCls} value={draft.name} onChange={(e) => set({ name: e.target.value })} placeholder="Checkout Platform" autoFocus />
              </Field>
              <Field label="Description">
                <input className={inputCls} value={draft.description} onChange={(e) => set({ description: e.target.value })} placeholder="Storefront, payments, inventory" />
              </Field>
              <label className="flex items-start gap-2 cursor-pointer pt-1">
                <input type="checkbox" className="mt-0.5 accent-primary" checked={draft.syncBeforeUp} onChange={(e) => set({ syncBeforeUp: e.target.checked })} />
                <span>
                  <span className="block font-label-md text-label-md text-on-surface">Safe-sync repos before Up</span>
                  <span className="block text-[11px] text-on-surface-variant">Fetch and fast-forward clean repos. Dirty or diverged repos are never touched.</span>
                </span>
              </label>
            </div>
          </Card>

          <Card title="Add infra" icon="inventory_2">
            <p className="text-[11px] text-on-surface-variant mb-space-sm">Runs as a Docker container with a persistent volume.</p>
            <div className="grid grid-cols-2 gap-1.5">
              {PLUGINS.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => setDraft((d) => (d ? { ...d, services: [...d.services, pluginToService(p, d.services.map((s) => s.name))] } : d))}
                  className="flex items-center gap-1.5 px-2 py-1.5 rounded-lg border border-outline-variant hover:border-primary hover:bg-surface-container-high text-left"
                >
                  <Icon name={p.icon} className="text-[16px] text-primary" />
                  <span className="font-label-md text-label-md text-on-surface">{p.name}</span>
                </button>
              ))}
            </div>
          </Card>

          {id && (
            <Card title="Danger zone" icon="warning">
              <ConfirmBtn
                icon="delete"
                variant="danger"
                confirmLabel="Really delete workspace"
                onConfirm={async () => {
                  await deleteWorkspace(id);
                  navigate("/workspaces");
                }}
              >
                Delete workspace
              </ConfirmBtn>
              <p className="text-[11px] text-outline mt-1.5">Stops running services. Repositories on disk are not touched.</p>
            </Card>
          )}
        </div>
      </div>
    </PageBody>
  );
}
