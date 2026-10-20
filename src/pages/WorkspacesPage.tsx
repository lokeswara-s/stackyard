import { Link, useNavigate } from "react-router-dom";

import { Btn, Card, ConfirmBtn, EmptyState, Icon, PageBody, PageHeader } from "../components/ui";
import { useApp } from "../state/AppState";

export default function WorkspacesPage() {
  const { store, active, setActive, deleteWorkspace } = useApp();
  const navigate = useNavigate();

  return (
    <PageBody>
      <PageHeader
        title="Workspaces"
        subtitle="Each workspace is a set of repositories and infra that start together."
        actions={
          <Btn variant="primary" icon="add" onClick={() => navigate("/workspaces/new")}>
            New workspace
          </Btn>
        }
      />
      {store.workspaces.length === 0 ? (
        <EmptyState icon="lan" title="No workspaces yet" action={<Btn variant="primary" icon="add" onClick={() => navigate("/workspaces/new")}>Create workspace</Btn>}>
          Connect your repositories and infra, set ports and env once, then start everything with one click.
        </EmptyState>
      ) : (
        <div className="grid gap-space-md" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(340px, 1fr))" }}>
          {store.workspaces.map((w) => {
            const isActive = w.id === active?.id;
            const apps = w.services.filter((s) => s.kind === "app").length;
            const infra = w.services.length - apps;
            return (
              <Card key={w.id} className={isActive ? "border-primary/50" : ""}>
                <div className="flex items-start justify-between">
                  <div>
                    <div className="flex items-center gap-2">
                      <Icon name="deployed_code" className="text-primary text-[20px]" />
                      <h3 className="font-headline-md text-headline-md text-on-surface">{w.name}</h3>
                      {isActive && <span className="text-[10px] px-1.5 py-0.5 rounded bg-primary/15 text-primary border border-primary/30">active</span>}
                    </div>
                    <p className="text-body-sm text-on-surface-variant mt-1">{w.description || "No description"}</p>
                  </div>
                </div>
                <div className="flex gap-4 my-space-md font-code-dense text-code-dense text-on-surface-variant">
                  <span>{apps} apps</span>
                  <span>{infra} infra</span>
                  <span>profile: {w.profile}</span>
                </div>
                <div className="flex flex-wrap gap-1 mb-space-md">
                  {w.services.slice(0, 8).map((s) => (
                    <span key={s.name} className="px-1.5 py-0.5 rounded bg-surface-container-highest text-[10px] font-code-dense text-on-surface-variant">
                      {s.name}
                    </span>
                  ))}
                  {w.services.length > 8 && <span className="text-[10px] text-outline">+{w.services.length - 8}</span>}
                </div>
                <div className="flex items-center gap-2">
                  {!isActive && (
                    <Btn icon="check" onClick={() => void setActive(w.id)}>
                      Make active
                    </Btn>
                  )}
                  <Link to={`/workspaces/${w.id}/edit`} className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-surface-container-high border border-outline-variant text-on-surface hover:bg-surface-container-highest font-label-md text-label-md">
                    <Icon name="edit" className="text-[16px]" /> Edit
                  </Link>
                  <ConfirmBtn icon="delete" className="ml-auto" confirmLabel="Confirm delete" onConfirm={() => void deleteWorkspace(w.id)}>
                    Delete
                  </ConfirmBtn>
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </PageBody>
  );
}
