import type { SVGProps } from "react";

export type IconProps = SVGProps<SVGSVGElement>;

const iconDefaults = {
  width: 18,
  height: 18,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

export function ControwlyMark(props: IconProps) {
  return (
    <svg {...iconDefaults} width="34" height="34" viewBox="0 0 34 34" {...props}>
      <rect x="1" y="1" width="32" height="32" rx="10" fill="currentColor" opacity="0.12" stroke="currentColor" />
      <path d="M11.4 17.3c0-3.25 2.54-5.9 5.7-5.9 1.75 0 3.32.81 4.36 2.08" stroke="currentColor" strokeWidth="2.2" />
      <path d="M11.4 16.7c0 3.25 2.54 5.9 5.7 5.9 1.75 0 3.32-.81 4.36-2.08" stroke="currentColor" strokeWidth="2.2" />
      <circle cx="11" cy="17" r="2" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function RefreshIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="M20 11a8.1 8.1 0 0 0-14.8-3L3 11" />
      <path d="M3 5v6h6" />
      <path d="M4 13a8.1 8.1 0 0 0 14.8 3L21 13" />
      <path d="M21 19v-6h-6" />
    </svg>
  );
}

export function BluetoothIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="m12 3 5 5-10 8 10 5-5 3V3Z" />
      <path d="m7 8 10 8" />
    </svg>
  );
}

export function CheckIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="m5 12 4 4L19 6" />
    </svg>
  );
}

export function CheckCircleIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="m8.5 12 2.2 2.2 4.8-5" />
    </svg>
  );
}

export function ChevronDownIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="m6 9 6 6 6-6" />
    </svg>
  );
}

export function CopyIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <rect x="8" y="8" width="11" height="12" rx="2" />
      <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h2" />
    </svg>
  );
}

export function InfoIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5" />
      <path d="M12 8h.01" />
    </svg>
  );
}

export function LockIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <rect x="5" y="10" width="14" height="10" rx="2" />
      <path d="M8 10V7a4 4 0 0 1 8 0v3" />
    </svg>
  );
}

export function MonitorIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <rect x="3" y="4" width="18" height="13" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  );
}

export function MoreIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <circle cx="5" cy="12" r="1" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" />
      <circle cx="19" cy="12" r="1" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function AlertIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="m12 3 9 17H3L12 3Z" />
      <path d="M12 9v4M12 16h.01" />
    </svg>
  );
}

export function XIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="m6 6 12 12M18 6 6 18" />
    </svg>
  );
}

export function ShieldIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="M12 3 20 6v5c0 5-3.4 8.4-8 10-4.6-1.6-8-5-8-10V6l8-3Z" />
      <path d="m8.5 12 2.2 2.2 4.8-5" />
    </svg>
  );
}

export function KeyboardIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <rect x="3" y="6" width="18" height="12" rx="2" />
      <path d="M6.5 10h.01M9.5 10h.01M12.5 10h.01M15.5 10h.01M18 10h.01M6.5 14h7M16.5 14h1" />
    </svg>
  );
}

export function LoaderIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props} className={props.className}>
      <path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.64 5.64l2.12 2.12M16.24 16.24l2.12 2.12M18.36 5.64l-2.12 2.12M7.76 16.24l-2.12 2.12" />
    </svg>
  );
}

export function GamepadIcon(props: IconProps) {
  return (
    <svg {...iconDefaults} {...props}>
      <path d="M6.5 9h11a4.5 4.5 0 0 1 4.28 5.9l-1.15 3.45a2.5 2.5 0 0 1-4.53.43l-1.2-2.03H9.1l-1.2 2.03a2.5 2.5 0 0 1-4.53-.43L2.22 14.9A4.5 4.5 0 0 1 6.5 9Z" />
      <path d="M7 12v4M5 14h4M16.5 13h.01M19 15h.01" />
    </svg>
  );
}
