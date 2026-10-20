import { useEffect, useState } from "react";

import { StorageCard } from "../components/StorageCard";
import { Btn, Card, Field, PageBody, PageHeader, inputCls } from "../components/ui";
import { api, isTauri } from "../lib/tauri";
import type { DockerOverview } from "../lib/types";
import { useApp } from "../state/AppState";

export default function SettingsPage() {
  const { store, saveSettings } = useApp();
  const [s, setS] = useState(store.settings);
  const [docker, setDocker] = useState<DockerOverview | null>(null);

  useEffect(() => setS(store.settings), [store.settings]);
  useEffect(() => {
    if (!isTauri()) return;
    api.dockerOverview().then(setDocker).catch(() => {});
  }, []);

  const dirty = JSON.stringify(s) !== JSON.stringify(store.settings);

  return (
    <PageBody>
      <PageHeader
        title="Settings"
        actions={
          <>
            <Btn variant="ghost" disabled={!dirty} onClick={() => setS(store.settings)}>Discard</Btn>
            <Btn variant="primary" icon="save" disabled={!dirty} onClick={() => void saveSettings(s)}>Save</Btn>
          </>
        }
      />
      <div className="grid gap-space-md max-w-3xl">
        <Card title="Up pipeline" icon="play_circle">
          <div className="space-y-space-md">
            <Field label="Health timeout (seconds)" hint="How long Up waits for a service's port to open before marking it failed.">
              <input className={`${inputCls} !w-32`} inputMode="numeric" value={s.healthTimeoutSecs} onChange={(e) => setS({ ...s, healthTimeoutSecs: Math.max(5, Math.min(600, Number(e.target.value.replace(/\D/g, "")) || 5)) })} />
            </Field>
            <label className="flex items-start gap-2 cursor-pointer">
              <input type="checkbox" className="mt-0.5 accent-primary" checked={s.stopOnExit} onChange={(e) => setS({ ...s, stopOnExit: e.target.checked })} />
              <span>
                <span className="block font-label-md text-label-md text-on-surface">Stop services when Stackyard quits</span>
                <span className="block text-[11px] text-on-surface-variant">Otherwise processes and Docker containers started by the app keep running after you close it.</span>
              </span>
            </label>
          </div>
        </Card>

        <Card title="Docker" icon="inventory_2">
          {!docker ? (
            <div className="text-body-sm text-on-surface-variant">Checking…</div>
          ) : docker.available ? (
            <div className="text-body-sm text-on-surface">
              <span className="text-secondary">●</span> Connected: Docker Engine <span className="font-code-dense">{docker.version}</span> · {docker.containers.length} containers · {docker.images.length} images
            </div>
          ) : (
            <div className="text-body-sm text-tertiary">{docker.error}</div>
          )}
        </Card>

        <StorageCard />

        <Card title="Data" icon="storage">
          <p className="text-body-sm text-on-surface-variant">
            Workspaces, services and environment variables are stored locally in the app data folder as <code>store.json</code>. Secret values are stored in
            plain text there. Do not put production credentials into a workspace profile.
          </p>
        </Card>
      </div>
    </PageBody>
  );
}
