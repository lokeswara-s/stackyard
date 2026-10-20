export interface NavItem {
  to: string;
  icon: string;
  label: string;
  end?: boolean;
}

export const PRIMARY_NAV: NavItem[] = [
  { to: "/", icon: "dashboard", label: "Services & Dashboard", end: true },
  { to: "/dependency-graph", icon: "account_tree", label: "Dependency Graph" },
  { to: "/workspaces", icon: "lan", label: "Workspaces" },
  { to: "/containers-volumes", icon: "inventory_2", label: "Containers & Volumes" },
  { to: "/ports-proxies", icon: "router", label: "Ports" },
  { to: "/git", icon: "fork_right", label: "Git & Multi-repo" },
  { to: "/logs", icon: "segment", label: "Logs" },
  { to: "/settings", icon: "tune", label: "Settings" },
];

export const TOOLS_NAV: NavItem[] = [
  { to: "/pipelines", icon: "conversion_path", label: "CI Pipelines" },
  { to: "/environment", icon: "vpn_key", label: "Environment" },
  { to: "/plugins", icon: "extension", label: "Plugins" },
  { to: "/fault-injection", icon: "bolt", label: "Fault Injection" },
];
