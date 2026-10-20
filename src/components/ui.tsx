import { useEffect, useState, type ButtonHTMLAttributes, type ReactNode } from "react";

import type { RuntimeStatus, ServiceRuntime, StepStatus } from "../lib/types";

export function Icon({ name, className = "" }: { name: string; className?: string }) {
  return <span className={`material-symbols-outlined ${className}`}>{name}</span>;
}

/** Lifecycle status, plus "degraded": running but logging errors. */
export type DisplayStatus = RuntimeStatus | "degraded";

export function displayStatus(rt?: ServiceRuntime | null): DisplayStatus {
  if (!rt) return "idle";
  return rt.status === "running" && rt.health === "degraded" ? "degraded" : rt.status;
}

export const STATUS_UI: Record<DisplayStatus, { label: string; text: string; dot: string; pill: string }> = {
  running: { label: "Healthy", text: "text-secondary", dot: "bg-secondary", pill: "bg-secondary/10 text-secondary border-secondary/30" },
  degraded: { label: "Degraded", text: "text-tertiary", dot: "bg-tertiary", pill: "bg-tertiary/15 text-tertiary border-tertiary/40" },
  starting: { label: "Starting", text: "text-tertiary", dot: "bg-tertiary animate-pulse", pill: "bg-tertiary/10 text-tertiary border-tertiary/30" },
  stopping: { label: "Stopping", text: "text-tertiary", dot: "bg-tertiary animate-pulse", pill: "bg-tertiary/10 text-tertiary border-tertiary/30" },
  failed: { label: "Failed", text: "text-error", dot: "bg-error", pill: "bg-error/10 text-error border-error/30" },
  stopped: { label: "Stopped", text: "text-outline", dot: "bg-outline", pill: "bg-surface-container-highest text-outline border-outline-variant" },
  idle: { label: "Not started", text: "text-outline", dot: "bg-outline-variant", pill: "bg-surface-container-highest text-outline border-outline-variant" },
};

export function StatusPill({ status }: { status: DisplayStatus }) {
  const s = STATUS_UI[status];
  return (
    <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded border text-[10px] font-label-sm ${s.pill}`}>
      {status === "degraded" ? <Icon name="warning" className="text-[11px]" /> : <span className={`w-1.5 h-1.5 rounded-full ${s.dot}`} />}
      {s.label}
    </span>
  );
}

export const STEP_ICON: Record<StepStatus, { icon: string; cls: string }> = {
  pending: { icon: "radio_button_unchecked", cls: "text-outline" },
  running: { icon: "progress_activity", cls: "text-primary animate-spin" },
  passed: { icon: "check_circle", cls: "text-secondary" },
  warn: { icon: "warning", cls: "text-tertiary" },
  failed: { icon: "error", cls: "text-error" },
  skipped: { icon: "remove_circle_outline", cls: "text-outline" },
};

type BtnVariant = "primary" | "secondary" | "ghost" | "danger" | "success";
const BTN: Record<BtnVariant, string> = {
  primary: "bg-primary text-on-primary hover:brightness-110",
  success: "bg-secondary text-on-secondary hover:brightness-110",
  secondary: "bg-surface-container-high border border-outline-variant text-on-surface hover:bg-surface-container-highest",
  ghost: "text-on-surface-variant hover:text-on-surface hover:bg-surface-container-high",
  danger: "bg-error-container/40 border border-error/40 text-error hover:bg-error-container/70",
};

export function Btn({
  variant = "secondary",
  icon,
  children,
  className = "",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: BtnVariant; icon?: string }) {
  return (
    <button
      type="button"
      {...rest}
      className={`inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg font-label-md text-label-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${BTN[variant]} ${className}`}
    >
      {icon && <Icon name={icon} className="text-[16px]" />}
      {children}
    </button>
  );
}

/** Two-step destructive button: first click arms it, second click confirms. */
export function ConfirmBtn({
  onConfirm,
  children,
  confirmLabel = "Click again to confirm",
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "onClick"> & {
  onConfirm: () => void;
  confirmLabel?: string;
  variant?: BtnVariant;
  icon?: string;
}) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 3500);
    return () => clearTimeout(t);
  }, [armed]);
  return (
    <Btn
      {...rest}
      variant={armed ? "danger" : (rest.variant ?? "ghost")}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else setArmed(true);
      }}
    >
      {armed ? confirmLabel : children}
    </Btn>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-space-lg mb-space-lg">
      <div>
        <h1 className="font-headline-lg text-headline-lg text-on-surface">{title}</h1>
        {subtitle && <p className="font-body-sm text-body-sm text-on-surface-variant mt-0.5">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-space-sm shrink-0">{actions}</div>}
    </div>
  );
}

export function Card({ title, icon, actions, children, className = "", bodyClass = "" }: {
  title?: ReactNode;
  icon?: string;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClass?: string;
}) {
  return (
    <section className={`bg-surface-container border border-surface-container-highest rounded-lg ${className}`}>
      {title && (
        <header className="flex items-center justify-between px-space-md py-space-sm border-b border-surface-container-highest">
          <h2 className="flex items-center gap-1.5 font-headline-sm text-headline-sm text-on-surface">
            {icon && <Icon name={icon} className="text-[18px] text-on-surface-variant" />}
            {title}
          </h2>
          {actions}
        </header>
      )}
      <div className={bodyClass || "p-space-md"}>{children}</div>
    </section>
  );
}

export function EmptyState({ icon, title, children, action }: { icon: string; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center text-center py-16 px-6 border border-dashed border-outline-variant rounded-lg bg-surface-container-low/40">
      <Icon name={icon} className="text-[40px] text-outline mb-3" />
      <h3 className="font-headline-md text-headline-md text-on-surface mb-1">{title}</h3>
      {children && <div className="font-body-sm text-body-sm text-on-surface-variant max-w-md mb-4">{children}</div>}
      {action}
    </div>
  );
}

export const inputCls =
  "w-full h-8 px-2 bg-surface-container-lowest border border-surface-container-highest rounded-lg text-body-sm font-body-sm text-on-surface placeholder:text-outline focus:outline-none focus:border-primary select-text";

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="block">
      <span className="block font-label-sm text-label-sm text-on-surface-variant mb-1">{label}</span>
      {children}
      {hint && <span className="block text-[10px] text-outline mt-0.5">{hint}</span>}
    </label>
  );
}

export function PageBody({ children, wide }: { children: ReactNode; wide?: boolean }) {
  return <div className={`p-margin ${wide ? "" : "max-w-[1400px] mx-auto"} w-full`}>{children}</div>;
}

export function Banner({ icon = "info", tone = "info", children }: { icon?: string; tone?: "info" | "warn" | "error"; children: ReactNode }) {
  const cls =
    tone === "error"
      ? "bg-error-container/30 border-error/40 text-error"
      : tone === "warn"
        ? "bg-tertiary/10 border-tertiary/30 text-tertiary"
        : "bg-primary/10 border-primary/30 text-primary";
  return (
    <div className={`flex items-start gap-2 px-space-md py-space-sm rounded-lg border mb-space-md ${cls}`}>
      <Icon name={icon} className="text-[18px] mt-px" />
      <div className="font-body-sm text-body-sm text-on-surface">{children}</div>
    </div>
  );
}

export function formatUptime(startedMs?: number | null, now = Date.now()): string {
  if (!startedMs) return "–";
  const s = Math.max(0, Math.floor((now - startedMs) / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s % 60}s`;
  return `${s}s`;
}

export function timeAgo(ms?: number | null): string {
  if (!ms) return "never";
  const s = Math.floor((Date.now() - ms) / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
