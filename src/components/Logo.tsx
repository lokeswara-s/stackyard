interface LogoProps {
  className?: string;
}

/** Stackyard mark (assets/logo-mark.png, served from public/logo.png). */
export default function Logo({ className }: LogoProps) {
  return <img src="/logo.png" alt="Stackyard" draggable={false} className={`rounded-lg object-cover ${className ?? ""}`} />;
}
