import { useCallback, useEffect, useMemo, useState } from "react";

import { Banner, Btn, Icon, PageBody, PageHeader } from "../components/ui";
import { PLUGINS, pluginToService, type Plugin } from "../lib/catalog";
import { api } from "../lib/tauri";
import type { DockerOverview } from "../lib/types";
import { useApp } from "../state/AppState";

const CI_IMAGES: { image: string; note: string }[] = [
  { image: "catthehacker/ubuntu:act-latest", note: "Ubuntu with git, make, curl, node, python, docker CLI. Closest to a GitHub/GitLab Ubuntu runner (large)." },
  { image: "ubuntu:24.04", note: "Plain Ubuntu 24.04. Small; install tools in the job." },
  { image: "node:22", note: "Node.js 22 with npm/yarn." },
  { image: "node:20", note: "Node.js 20 with npm/yarn." },
  { image: "python:3.12", note: "Python 3.12 with pip." },
  { image: "ruby:3.3", note: "Ruby 3.3 with bundler." },
  { image: "golang:1.23", note: "Go 1.23 toolchain." },
  { image: "maven:3.9-eclipse-temurin-21", note: "Maven + JDK 21." },
  { image: "alpine:3.20", note: "Tiny Alpine Linux (musl, busybox)." },
];

/** `ubuntu` -> `ubuntu:latest`, as Docker lists it. */
const withTag = (image: string) => (/:[^/:]+$/.test(image) ? image : `${image}:latest`);

function CiImagesSection({ installed, dockerUp, onChanged }: { installed: Set<string>; dockerUp: boolean; onChanged: () => Promise<void> }) {
  const { store, saveSettings, toast } = useApp();
  const settings = store.settings;
  const custom = settings.ciImages ?? [];
  const def = settings.ciDefaultImage?.trim() || CI_IMAGES[0].image;
  const [busy, setBusy] = useState<string | null>(null);
  const [draft, setDraft] = useState("");

  const all = [...CI_IMAGES.map((i) => ({ ...i, custom: false })), ...custom.filter((c) => !CI_IMAGES.some((i) => i.image === c)).map((image) => ({ image, note: "Your image", custom: true }))];

  const act = async (image: string, kind: "pull" | "image_rm") => {
    setBusy(image);
    try {
      await api.dockerAction(kind, image);
      toast(kind === "pull" ? `${image} is ready; pipelines reuse it` : `${image} removed`, "success");
      await onChanged();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const addCustom = async () => {
    const image = draft.trim();
    if (!image || /\s/.test(image)) return;
    if (!all.some((i) => i.image === image)) await saveSettings({ ...settings, ciImages: [...custom, image] });
    setDraft("");
    void act(image, "pull");
  };

  return (
    <section className="mb-space-lg">
      <h2 className="font-label-sm text-label-sm uppercase tracking-wider text-outline mb-1">CI runner images</h2>
      <p className="text-[12px] text-on-surface-variant mb-space-sm">
        Pipelines (GitLab CI, GitHub Actions, Jenkins) run their jobs in these images. Pull one once here and every run reuses it; the default is used by jobs that do not name an image.
      </p>
      <div className="bg-surface-container border border-surface-container-highest rounded-lg divide-y divide-surface-container-highest">
        {all.map((i) => {
          const have = installed.has(withTag(i.image));
          const isDef = i.image === def;
          return (
            <div key={i.image} className="flex items-center gap-3 px-space-md py-space-sm">
              <Icon name="deployed_code" className="text-primary text-[20px]" />
              <div className="min-w-0 flex-1">
                <div className="font-code-dense text-code-dense text-on-surface truncate">
                  {i.image}
                  {isDef && <span className="ml-2 px-1.5 py-0.5 rounded bg-primary/15 text-primary text-[10px] font-label-sm">default</span>}
                </div>
                <div className="text-[11px] text-on-surface-variant truncate" title={i.note}>
                  {i.note}
                </div>
              </div>
              {dockerUp &&
                (have ? (
                  <span className="inline-flex items-center gap-1 text-[11px] text-secondary">
                    <Icon name="download_done" className="text-[14px]" />
                    ready
                  </span>
                ) : (
                  <Btn icon={busy === i.image ? "progress_activity" : "download"} className="!px-2 !py-1" disabled={busy !== null} onClick={() => void act(i.image, "pull")}>
                    {busy === i.image ? "Pulling…" : "Pull"}
                  </Btn>
                ))}
              {!isDef && (
                <Btn variant="ghost" icon="star" className="!px-2 !py-1" onClick={() => void saveSettings({ ...settings, ciDefaultImage: i.image === CI_IMAGES[0].image ? "" : i.image })}>
                  Use as default
                </Btn>
              )}
              {dockerUp && have && (
                <Btn variant="ghost" icon="delete" className="!px-2 !py-1" disabled={busy !== null} onClick={() => void act(i.image, "image_rm")} aria-label="Remove image" />
              )}
              {i.custom && (
                <Btn
                  variant="ghost"
                  icon="close"
                  className="!px-2 !py-1"
                  aria-label="Forget image"
                  onClick={() => void saveSettings({ ...settings, ciImages: custom.filter((c) => c !== i.image), ciDefaultImage: isDef ? "" : settings.ciDefaultImage })}
                />
              )}
            </div>
          );
        })}
        <div className="flex items-center gap-2 px-space-md py-space-sm">
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void addCustom()}
            placeholder="Add an image, e.g. registry.example.com/team/ci:1.4"
            className="h-8 flex-1 px-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg font-code-dense text-code-dense text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text"
          />
          <Btn icon="add" disabled={!draft.trim() || busy !== null} onClick={() => void addCustom()}>
            Add &amp; pull
          </Btn>
        </div>
      </div>
    </section>
  );
}

export default function PluginMarketplacePage() {
  const { active, saveWorkspace, toast } = useApp();
  const [docker, setDocker] = useState<DockerOverview | null>(null);
  const [pulling, setPulling] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  const refresh = useCallback(async () => {
    try {
      setDocker(await api.dockerOverview());
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    }
  }, [toast]);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const installed = useMemo(() => new Set((docker?.images ?? []).map((i) => `${i.Repository}:${i.Tag}`)), [docker]);
  const inWorkspace = (p: Plugin) => active?.services.some((s) => s.image === p.image) ?? false;

  const pull = async (p: Plugin) => {
    setPulling(p.id);
    try {
      await api.dockerAction("pull", p.image);
      toast(`${p.name} image pulled`, "success");
      await refresh();
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), "error");
    } finally {
      setPulling(null);
    }
  };

  const add = async (p: Plugin) => {
    if (!active) return;
    const svc = pluginToService(p, active.services.map((s) => s.name));
    if (await saveWorkspace({ ...active, services: [...active.services, svc] })) toast(`${p.name} added to ${active.name} as “${svc.name}”`, "success");
  };

  const q = query.trim().toLowerCase();
  const list = PLUGINS.filter((p) => !q || p.name.toLowerCase().includes(q) || p.category.toLowerCase().includes(q) || p.description.toLowerCase().includes(q));
  const categories = [...new Set(list.map((p) => p.category))];

  return (
    <PageBody>
      <PageHeader
        title="Plugins"
        subtitle="Docker-backed infra you can drop into a workspace. Images are pulled on demand."
        actions={
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search plugins…" className="h-8 w-56 px-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text" />
        }
      />
      {docker && !docker.available && (
        <Banner tone="warn" icon="warning">
          {docker.error} You can still add plugins to a workspace, but they won’t start until Docker is running.
        </Banner>
      )}
      {!active && <Banner icon="info">Create a workspace first to add plugins to it.</Banner>}

      {(!q || "ci runner images pipeline".includes(q) || CI_IMAGES.some((i) => i.image.includes(q))) && <CiImagesSection installed={installed} dockerUp={docker?.available ?? false} onChanged={refresh} />}

      {categories.map((cat) => (
        <section key={cat} className="mb-space-lg">
          <h2 className="font-label-sm text-label-sm uppercase tracking-wider text-outline mb-space-sm">{cat}</h2>
          <div className="grid gap-space-md" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))" }}>
            {list.filter((p) => p.category === cat).map((p) => {
              const have = installed.has(p.image);
              const used = inWorkspace(p);
              return (
                <div key={p.id} className="bg-surface-container border border-surface-container-highest rounded-lg p-space-md flex flex-col gap-space-sm">
                  <div className="flex items-start gap-3">
                    <div className="w-10 h-10 rounded-lg bg-surface-container-highest flex items-center justify-center">
                      <Icon name={p.icon} className="text-primary text-[22px]" />
                    </div>
                    <div className="min-w-0">
                      <h3 className="font-headline-sm text-headline-sm text-on-surface">{p.name}</h3>
                      <p className="text-[11px] text-on-surface-variant">{p.description}</p>
                    </div>
                  </div>
                  <div className="font-code-dense text-code-dense text-outline flex flex-wrap gap-x-3">
                    <span>{p.image}</span>
                    <span>:{p.port}</span>
                    {p.dataPath && <span>volume {p.dataPath}</span>}
                  </div>
                  <div className="flex items-center gap-2 mt-auto pt-space-xs border-t border-surface-container-highest">
                    {docker?.available &&
                      (have ? (
                        <span className="inline-flex items-center gap-1 text-[11px] text-secondary"><Icon name="download_done" className="text-[14px]" />image ready</span>
                      ) : (
                        <Btn icon={pulling === p.id ? "progress_activity" : "download"} className="!px-2 !py-1" disabled={pulling !== null} onClick={() => void pull(p)}>
                          {pulling === p.id ? "Pulling…" : "Pull image"}
                        </Btn>
                      ))}
                    <Btn variant={used ? "ghost" : "primary"} icon={used ? "check" : "add"} className="ml-auto !px-2 !py-1" disabled={!active || used} onClick={() => void add(p)}>
                      {used ? "In workspace" : "Add to workspace"}
                    </Btn>
                  </div>
                </div>
              );
            })}
          </div>
        </section>
      ))}
      {list.length === 0 && <div className="text-center py-12 text-on-surface-variant">No plugins match “{query}”.</div>}
    </PageBody>
  );
}
