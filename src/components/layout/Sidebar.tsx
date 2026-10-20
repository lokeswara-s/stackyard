import { NavLink, useMatch } from "react-router-dom";

import { PRIMARY_NAV, TOOLS_NAV, type NavItem } from "../../nav";
import { useApp } from "../../state/AppState";
import { Icon } from "../ui";

const BASE = "flex items-center gap-space-sm px-space-sm py-1.5 rounded-r-lg transition-colors";
const ACTIVE = "bg-surface-container-high text-primary border-l-2 border-primary font-medium";
const IDLE = "text-on-surface-variant hover:bg-surface-container hover:text-on-surface";

function Item({ item }: { item: NavItem }) {
  return (
    <NavLink to={item.to} end={item.end} className={({ isActive }) => `${BASE} ${isActive ? ACTIVE : IDLE}`}>
      <Icon name={item.icon} className="text-[18px]" />
      <span className="font-label-md text-label-md">{item.label}</span>
    </NavLink>
  );
}

function SectionLabel({ children }: { children: string }) {
  return <div className="px-space-sm py-space-xs text-[10px] font-label-sm tracking-wider uppercase text-outline">{children}</div>;
}

function ResourceBar({ label, value, fill, pct }: { label: string; value: string; fill: string; pct: number }) {
  return (
    <div className="space-y-0.5">
      <div className="flex justify-between text-[10px] font-code-dense text-on-surface-variant">
        <span>{label}</span>
        <span>{value}</span>
      </div>
      <div className="w-full h-1 bg-surface-container-highest rounded-full overflow-hidden">
        <div className={`h-full ${fill} rounded-full transition-all`} style={{ width: `${Math.min(100, Math.max(0, pct))}%` }} />
      </div>
    </div>
  );
}

export default function Sidebar() {
  const serviceMatch = useMatch("/services/:name");
  const { stats, active, runtime } = useApp();
  const [dashboard, ...restPrimary] = PRIMARY_NAV;

  const running = Object.values(runtime).filter((r) => r.status === "running").length;
  const total = active?.services.length ?? 0;
  const memPct = stats ? (stats.memUsedGb / Math.max(stats.memTotalGb, 0.1)) * 100 : 0;
  const load = stats ? Math.round(stats.cpu) : 0;

  return (
    <aside className="fixed left-0 top-14 bottom-6 w-64 bg-surface-container-low border-r border-surface-container-highest z-40 flex flex-col justify-between">
      <div className="p-space-sm overflow-y-auto">
        <SectionLabel>Navigation</SectionLabel>
        <nav className="space-y-0.5">
          <Item item={dashboard} />
          {serviceMatch && (
            <div className="pl-space-md py-0.5 space-y-0.5">
              <NavLink
                to={serviceMatch.pathname}
                className={({ isActive }) =>
                  `flex items-center gap-space-xs px-space-sm py-1 rounded-r text-[11px] font-code-dense transition-colors ${isActive ? ACTIVE : IDLE}`
                }
              >
                {decodeURIComponent(serviceMatch.params.name ?? "")}
              </NavLink>
            </div>
          )}
          {restPrimary.map((item) => (
            <Item key={item.to} item={item} />
          ))}
        </nav>

        <div className="mt-space-md">
          <SectionLabel>Tools</SectionLabel>
          <nav className="space-y-0.5">
            {TOOLS_NAV.map((item) => (
              <Item key={item.to} item={item} />
            ))}
          </nav>
        </div>
      </div>

      <div className="p-space-sm border-t border-surface-container-highest bg-surface-container-lowest/60">
        <div className="flex items-center justify-between mb-space-xs">
          <span className="font-label-sm text-label-sm text-outline">This machine</span>
          <span className="font-code-dense text-code-dense text-secondary">{stats ? `${load}% LOAD` : "…"}</span>
        </div>
        <div className="space-y-1.5 mb-space-sm">
          <ResourceBar label="CPU" value={stats ? `${stats.cpu.toFixed(0)}% · ${stats.cores} cores` : "…"} fill="bg-primary" pct={stats?.cpu ?? 0} />
          <ResourceBar label="RAM" value={stats ? `${stats.memUsedGb.toFixed(1)} / ${stats.memTotalGb.toFixed(0)} GB` : "…"} fill="bg-secondary" pct={memPct} />
        </div>
        <div className="flex items-center justify-between pt-space-xs border-t border-surface-container border-dashed text-[10px] font-code-dense text-outline">
          <span>{active ? `profile: ${active.profile}` : "no workspace"}</span>
          <span>
            {running}/{total} up
          </span>
        </div>
      </div>
    </aside>
  );
}
