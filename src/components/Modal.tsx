import { useEffect, type ReactNode } from "react";

import { Icon } from "./ui";

/** Small centered dialog. Closes on Esc and on a click outside. */
export function Modal({ title, icon, onClose, children, footer, width = 460 }: { title: string; icon?: string; onClose: () => void; children: ReactNode; footer?: ReactNode; width?: number }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div role="dialog" aria-label={title} style={{ width }} className="max-w-full max-h-[85vh] flex flex-col bg-surface-container border border-surface-container-highest rounded-lg shadow-2xl">
        <header className="flex items-center gap-2 px-space-md py-space-sm border-b border-surface-container-highest">
          {icon && <Icon name={icon} className="text-[18px] text-on-surface-variant" />}
          <h2 className="flex-1 font-headline-sm text-headline-sm text-on-surface truncate">{title}</h2>
          <button type="button" onClick={onClose} className="text-outline hover:text-on-surface" aria-label="Close">
            <Icon name="close" className="text-[18px]" />
          </button>
        </header>
        <div className="p-space-md overflow-auto">{children}</div>
        {footer && <footer className="flex items-center justify-end gap-2 px-space-md py-space-sm border-t border-surface-container-highest">{footer}</footer>}
      </div>
    </div>
  );
}
