import { Link } from "react-router-dom";

import { useApp } from "../../state/AppState";
import { Icon, displayStatus } from "../ui";

const Divider = () => <div className="h-3 w-[1px] bg-outline-variant" />;

export default function StatusBar() {
  const { active, runtime, stats, up } = useApp();
  const list = Object.values(runtime);
  const running = list.filter((r) => displayStatus(r) === "running").length;
  const degraded = list.filter((r) => displayStatus(r) === "degraded").length;
  const failed = list.filter((r) => r.status === "failed").length;
  const total = active?.services.length ?? 0;
  const openPorts = list.filter((r) => r.portOpen).length;

  return (
    <footer className="fixed bottom-0 left-0 right-0 h-6 bg-surface-container-lowest border-t border-surface-container-highest z-50 px-space-sm flex items-center justify-between font-code-dense text-code-dense text-on-surface-variant">
      <div className="flex items-center gap-space-md">
        <Link to="/" className="flex items-center gap-1 hover:text-on-surface">
          <Icon
            name={failed ? "error" : degraded ? "warning" : "check_circle"}
            className={`text-[14px] ${failed ? "text-error" : degraded ? "text-tertiary" : "text-secondary"}`}
          />
          <span>
            {running}/{total} healthy{degraded ? ` · ${degraded} degraded` : ""}
            {failed ? ` · ${failed} failed` : ""}
          </span>
        </Link>
        {up?.status === "running" && (
          <>
            <Divider />
            <Link to="/up" className="flex items-center gap-1 text-primary hover:text-on-surface">
              <Icon name="progress_activity" className="text-[14px] animate-spin" />
              <span>Up in progress</span>
            </Link>
          </>
        )}
        {active && (
          <>
            <Divider />
            <span>profile: {active.profile}</span>
          </>
        )}
      </div>
      <div className="flex items-center gap-space-md">
        <Link to="/ports-proxies" className="flex items-center gap-1 hover:text-on-surface">
          <Icon name="lan" className="text-[14px] text-outline" />
          <span>{openPorts} open ports</span>
        </Link>
        <Divider />
        <div className="flex items-center gap-1">
          <Icon name="memory" className="text-[14px] text-outline" />
          <span>{stats ? `${stats.memUsedGb.toFixed(1)}GB / ${stats.memTotalGb.toFixed(0)}GB` : "…"}</span>
        </div>
      </div>
    </footer>
  );
}
