import { Suspense, type MouseEvent } from "react";
import { Outlet } from "react-router-dom";

import { openExternal } from "../../lib/tauri";
import { useApp } from "../../state/AppState";
import { Icon } from "../ui";
import Header from "./Header";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";

/** http(s) links with target=_blank open in the system browser (webviews ignore target=_blank). */
function handleLinkClick(e: MouseEvent<HTMLDivElement>) {
  const anchor = (e.target as HTMLElement).closest("a");
  if (!anchor) return;
  const href = anchor.getAttribute("href");
  if (href === "#") {
    e.preventDefault();
    return;
  }
  if (anchor.target === "_blank" && href && /^https?:\/\//i.test(href)) {
    e.preventDefault();
    void openExternal(href);
  }
}

export default function AppShell() {
  const { ready, fatal } = useApp();
  return (
    <div onClick={handleLinkClick}>
      <Header />
      <Sidebar />
      <div className="pl-64">
        <main className="w-full pt-14 pb-6 min-h-screen bg-surface">
          {!ready ? (
            <div className="p-10 text-on-surface-variant font-body-sm">Loading…</div>
          ) : fatal ? (
            <div className="m-margin p-space-xl rounded-lg border border-error/40 bg-error-container/20 flex gap-3">
              <Icon name="desktop_access_disabled" className="text-error text-[28px]" />
              <div>
                <h2 className="font-headline-md text-headline-md text-on-surface mb-1">Backend not available</h2>
                <p className="font-body-md text-body-md text-on-surface-variant select-text">{fatal}</p>
              </div>
            </div>
          ) : (
            <Suspense fallback={null}>
              <Outlet />
            </Suspense>
          )}
        </main>
      </div>
      <StatusBar />
    </div>
  );
}
