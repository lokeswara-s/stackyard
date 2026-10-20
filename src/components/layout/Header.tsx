import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router-dom";

import Logo from "../Logo";
import { Icon } from "../ui";
import { closeWindow, minimizeWindow, toggleMaximizeWindow } from "../../lib/tauri";
import { PROFILES } from "../../lib/types";
import { useApp } from "../../state/AppState";
import { PRIMARY_NAV, TOOLS_NAV } from "../../nav";

const SHORT: Record<string, string> = { local: "Local", "staging-like": "Staging", "prod-sim": "Prod-sim" };

function useClickAway(ref: React.RefObject<HTMLElement | null>, onAway: () => void, active: boolean) {
  useEffect(() => {
    if (!active) return;
    const h = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onAway();
    };
    window.addEventListener("mousedown", h);
    return () => window.removeEventListener("mousedown", h);
  }, [ref, onAway, active]);
}

export default function Header() {
  const { store, active, setActive, setProfile } = useApp();
  const navigate = useNavigate();

  const [wsOpen, setWsOpen] = useState(false);
  const wsRef = useRef<HTMLDivElement>(null);
  useClickAway(wsRef, () => setWsOpen(false), wsOpen);

  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const searchBox = useRef<HTMLDivElement>(null);
  useClickAway(searchBox, () => setSearchOpen(false), searchOpen);

  // Cmd/Ctrl+K focuses the global search.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchRef.current?.focus();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return [];
    const svc = (active?.services ?? [])
      .filter((s) => s.name.toLowerCase().includes(q) || (s.port ? String(s.port).includes(q) : false))
      .map((s) => ({ label: s.name, hint: s.port ? `:${s.port}` : s.kind, icon: "deployed_code", to: `/services/${encodeURIComponent(s.name)}` }));
    const pages = [...PRIMARY_NAV, ...TOOLS_NAV]
      .filter((n) => n.label.toLowerCase().includes(q))
      .map((n) => ({ label: n.label, hint: "page", icon: n.icon, to: n.to }));
    return [...svc, ...pages].slice(0, 8);
  }, [query, active]);

  const go = (to: string) => {
    navigate(to);
    setQuery("");
    setSearchOpen(false);
    searchRef.current?.blur();
  };

  return (
    <header
      data-tauri-drag-region
      className="fixed top-0 left-0 right-0 h-14 bg-surface-container-low border-b border-surface-container-highest z-50 px-space-md flex items-center justify-between"
    >
      <div data-tauri-drag-region className="flex items-center gap-space-md">
        <div className="group flex items-center gap-1.5 mr-space-xs">
          <button type="button" aria-label="Close window" title="Close" onClick={closeWindow} className="w-3 h-3 rounded-full bg-[#ff5f56] inline-block hover:brightness-110" />
          <button type="button" aria-label="Minimize window" title="Minimize" onClick={minimizeWindow} className="w-3 h-3 rounded-full bg-[#ffbd2e] inline-block hover:brightness-110" />
          <button type="button" aria-label="Maximize window" title="Maximize" onClick={toggleMaximizeWindow} className="w-3 h-3 rounded-full bg-[#27c93f] inline-block hover:brightness-110" />
        </div>

        <Link to="/" className="flex items-center gap-space-sm pl-space-xs">
          <Logo className="h-8 w-8" />
          <span className="font-headline-sm text-headline-sm text-on-surface tracking-tight hidden sm:inline-block">Stackyard</span>
        </Link>

        <div className="h-4 w-[1px] bg-outline-variant mx-space-xs" />

        <div ref={wsRef} className="relative">
          <button
            type="button"
            onClick={() => setWsOpen((o) => !o)}
            className="flex items-center gap-space-xs px-space-sm py-1 bg-surface-container rounded-lg border border-surface-container-highest hover:bg-surface-container-high transition-colors"
          >
            <Icon name="deployed_code" className="text-primary text-[18px]" />
            <span className="font-label-md text-label-md text-on-surface">{active?.name ?? "No workspace"}</span>
            {active && (
              <span className="text-[10px] font-label-sm px-1.5 py-0.5 rounded bg-surface-container-highest text-secondary border border-secondary/30">
                {active.services.length} services
              </span>
            )}
            <Icon name="unfold_more" className="text-on-surface-variant text-[16px]" />
          </button>
          {wsOpen && (
            <div className="absolute top-full left-0 mt-1 w-72 bg-surface-container-high border border-outline-variant rounded-lg shadow-2xl py-1 z-50">
              {store.workspaces.length === 0 && <div className="px-3 py-2 text-body-sm text-outline">No workspaces yet</div>}
              {store.workspaces.map((w) => (
                <button
                  key={w.id}
                  type="button"
                  onClick={() => {
                    void setActive(w.id);
                    setWsOpen(false);
                    navigate("/");
                  }}
                  className="w-full flex items-center justify-between px-3 py-1.5 text-left hover:bg-surface-container-highest"
                >
                  <span className="font-label-md text-label-md text-on-surface">{w.name}</span>
                  <span className="flex items-center gap-2 text-[10px] text-outline">
                    {w.services.length} svc
                    {w.id === active?.id && <Icon name="check" className="text-primary text-[16px]" />}
                  </span>
                </button>
              ))}
              <div className="border-t border-outline-variant mt-1 pt-1">
                <button
                  type="button"
                  onClick={() => {
                    setWsOpen(false);
                    navigate("/workspaces/new");
                  }}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-left text-primary hover:bg-surface-container-highest font-label-md text-label-md"
                >
                  <Icon name="add" className="text-[16px]" /> New workspace
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setWsOpen(false);
                    navigate("/workspaces");
                  }}
                  className="w-full flex items-center gap-2 px-3 py-1.5 text-left text-on-surface-variant hover:bg-surface-container-highest font-label-md text-label-md"
                >
                  <Icon name="settings" className="text-[16px]" /> Manage workspaces
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      <div data-tauri-drag-region className="flex-1 max-w-md mx-space-lg hidden md:block">
        <div ref={searchBox} className="relative flex items-center w-full">
          <Icon name="search" className="absolute left-space-sm text-on-surface-variant text-[18px]" />
          <input
            ref={searchRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSearchOpen(true);
            }}
            onFocus={() => setSearchOpen(true)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && results[0]) go(results[0].to);
              if (e.key === "Escape") {
                setQuery("");
                setSearchOpen(false);
                searchRef.current?.blur();
              }
            }}
            className="w-full h-8 pl-8 pr-12 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm font-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text"
            placeholder="Search services, ports, pages..."
            type="text"
          />
          <kbd className="absolute right-space-sm px-1.5 py-0.5 text-[10px] font-label-sm bg-surface-container-high text-on-surface-variant rounded border border-surface-container-highest">
            Ctrl K
          </kbd>
          {searchOpen && query.trim() && (
            <div className="absolute top-full left-0 right-0 mt-1 bg-surface-container-high border border-outline-variant rounded-lg shadow-2xl py-1 z-50">
              {results.length === 0 && <div className="px-3 py-2 text-body-sm text-outline">No matches</div>}
              {results.map((r) => (
                <button key={r.to + r.label} type="button" onClick={() => go(r.to)} className="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-surface-container-highest">
                  <Icon name={r.icon} className="text-[16px] text-on-surface-variant" />
                  <span className="font-label-md text-label-md text-on-surface flex-1">{r.label}</span>
                  <span className="text-[10px] text-outline">{r.hint}</span>
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      <div data-tauri-drag-region className="flex items-center gap-space-sm">
        <div className="flex items-center bg-surface-container-lowest p-0.5 rounded-lg border border-surface-container-highest" title="Environment profile used when starting services">
          {PROFILES.map((p) => (
            <button
              key={p.id}
              type="button"
              disabled={!active}
              onClick={() => void setProfile(p.id)}
              title={p.hint}
              className={
                p.id === active?.profile
                  ? "px-2 py-1 text-label-sm font-label-sm rounded bg-surface-container-high text-primary"
                  : "px-2 py-1 text-label-sm font-label-sm rounded text-on-surface-variant hover:text-on-surface disabled:opacity-40"
              }
            >
              {SHORT[p.id]}
            </button>
          ))}
        </div>
      </div>
    </header>
  );
}
