import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { ControllerDevice, ControllerState, DashboardPhase, PendingOperation, UpdateState, UpdateStatus } from "@/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Separator } from "@/components/ui/separator";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  AlertIcon,
  BluetoothIcon,
  CheckCircleIcon,
  CheckIcon,
  ControwlyMark,
  CopyIcon,
  GamepadIcon,
  InfoIcon,
  KeyboardIcon,
  LoaderIcon,
  LockIcon,
  MonitorIcon,
  RefreshIcon,
  ShieldIcon,
  XIcon,
} from "@/components/icons";
import {
  controllerApi,
  formatError,
  isTauriRuntime,
} from "@/lib/tauri";
import { cn } from "@/lib/utils";

 type UiError = {
  title: string;
  detail: string;
};

type Platform = "windows" | "linux" | "other";

type ErrorCopy = {
  title: string;
  guidance: string;
};

const ERROR_HINTS: Array<{ code: string; copy: ErrorCopy }> = [
  {
    code: "authorization_required",
    copy: {
      title: "Authorization required",
      guidance: "Approve the operating system authorization prompt, then retry the action.",
    },
  },
  {
    code: "permission_denied",
    copy: {
      title: "Permission denied",
      guidance: "The operating system did not allow this change. Check the app permission guidance and try again.",
    },
  },
  {
    code: "stale_identity",
    copy: {
      title: "Device identity changed",
      guidance: "The controller may have reconnected with a new identity. Refresh the inventory before trying again.",
    },
  },
  {
    code: "device_not_found",
    copy: {
      title: "Controller is no longer connected",
      guidance: "Reconnect the controller, or refresh to remove the stale inventory record.",
    },
  },
  {
    code: "unsupported",
    copy: {
      title: "Capability not supported",
      guidance: "The native backend could identify the controller, but this operation is not available for its capabilities.",
    },
  },
  {
    code: "partial",
    copy: {
      title: "Partial result",
      guidance: "Some controllers may have changed while others could not. Review each row and retry only what is needed.",
    },
  },
];

const UPDATE_COPY: Record<UpdateStatus, { label: string; detail: string }> = {
  idle: { label: "Update service idle", detail: "Check when you want to look for a newer release." },
  checking: { label: "Checking for updates", detail: "Contacting the release service…" },
  current: { label: "You are up to date", detail: "No newer release was reported." },
  available: { label: "Update available", detail: "Review the version, then choose Install explicitly." },
  downloading: { label: "Downloading update", detail: "Keep Controwly open while the package downloads." },
  installing: { label: "Installing update", detail: "Controwly may restart when installation completes." },
  error: { label: "Update check needs attention", detail: "The update service returned an error." },
};

const PAIRING_GUIDES = [
  {
    title: "Modern Xbox Wireless controller",
    detail: "For models with Bluetooth, turn it on, then hold the Pair button until the Xbox logo flashes. Select it in your operating system’s Bluetooth manager. Xbox 360 and early Xbox One models need USB or their proprietary wireless adapter instead.",
  },
  {
    title: "DualSense or DualShock 4",
    detail: "For DualSense, hold PS + Create. For DualShock 4, hold PS + Share. Release when the light bar flashes, then select Wireless Controller.",
  },
  {
    title: "Another controller",
    detail: "Use the manufacturer’s pairing instructions, then select the controller’s device name in Bluetooth settings. Some wireless controllers require their included USB receiver instead.",
  },
];

function getPlatform(): Platform {
  if (typeof navigator === "undefined") return "other";
  const platform = `${navigator.platform} ${navigator.userAgent}`.toLowerCase();
  if (platform.includes("win")) return "windows";
  if (platform.includes("linux")) return "linux";
  return "other";
}

function getAuthorizationGuidance(platform: Platform): string {
  if (platform === "windows") {
    return "Windows may show a User Account Control prompt for system-level changes. Approve it only when you initiated the action in Controwly.";
  }
  if (platform === "linux") {
    return "Your Linux desktop or policy service may ask for administrator authorization. If permission is denied, review that policy before retrying.";
  }
  return "Your operating system may ask for administrator authorization for system-level changes. Controwly never silently elevates your permissions.";
}

function getErrorCopy(detail: string): ErrorCopy {
  const normalized = detail.toLowerCase();
  const match = ERROR_HINTS.find(({ code }) => normalized.includes(code));
  return match?.copy ?? {
    title: "Backend reported an issue",
    guidance: "The native backend could not complete the request. Refresh the inventory and try again if the problem persists.",
  };
}

function formatUpdatedAt(value: Date | null): string {
  if (!value) return "Waiting for the first inventory snapshot";
  return `Updated ${value.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
}

function formatBytes(value: number): string {
  if (value < 1024) return `${Math.round(value)} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function StatusPill({ phase, hasWarnings }: { phase: DashboardPhase; hasWarnings: boolean }) {
  const status = phase === "loading"
    ? { label: "Connecting", tone: "bg-amber-300", text: "text-amber-200" }
    : phase === "unsupported"
      ? { label: "Desktop app required", tone: "bg-slate-400", text: "text-slate-300" }
      : phase === "error"
        ? { label: "Backend unavailable", tone: "bg-red-400", text: "text-red-200" }
        : hasWarnings
          ? { label: "Needs attention", tone: "bg-amber-300", text: "text-amber-200" }
          : { label: "Backend connected", tone: "bg-primary", text: "text-primary" };

  return (
    <div className={cn("inline-flex items-center gap-2 rounded-full border border-border/80 bg-card/75 px-3 py-1.5 text-xs font-semibold", status.text)} role="status" aria-live="polite">
      <span className={cn("status-dot", status.tone, phase === "loading" && "animate-pulse")} />
      {status.label}
    </div>
  );
}

function MetricCard({ label, value, detail, tone }: { label: string; value: number; detail: string; tone: "primary" | "amber" | "violet" }) {
  const toneClass = tone === "primary"
    ? "text-primary"
    : tone === "amber"
      ? "text-amber-200"
      : "text-violet-200";

  return (
    <Card className="panel-highlight border-border/70 bg-card/85">
      <CardContent className="flex items-end justify-between gap-3 p-5">
        <div>
          <p className="eyebrow">{label}</p>
          <p className={cn("mt-2 text-3xl font-semibold tracking-tight", toneClass)}>{value}</p>
        </div>
        <p className="max-w-[8rem] text-right text-xs leading-relaxed text-muted-foreground">{detail}</p>
      </CardContent>
    </Card>
  );
}

function ErrorBanner({ error, onDismiss }: { error: UiError; onDismiss?: () => void }) {
  const copy = getErrorCopy(error.detail);
  return (
    <div className="flex gap-3 rounded-xl border border-red-400/25 bg-red-500/[0.08] p-4 text-sm" role="alert">
      <AlertIcon className="mt-0.5 shrink-0 text-red-300" />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <p className="font-semibold text-red-100">{error.title || copy.title}</p>
          {onDismiss ? (
            <Button type="button" variant="ghost" size="icon" className="-mr-2 -mt-2 h-8 w-8 text-red-200 hover:bg-red-400/10 hover:text-red-100" onClick={onDismiss} aria-label="Dismiss error">
              <XIcon className="h-4 w-4" />
            </Button>
          ) : null}
        </div>
        <p className="mt-1 leading-relaxed text-red-100/75">{copy.guidance}</p>
        <details className="mt-2 text-xs text-red-100/60">
          <summary className="cursor-pointer select-none font-medium hover:text-red-100">Technical detail</summary>
          <p className="mt-1 break-words font-mono">{error.detail}</p>
        </details>
      </div>
    </div>
  );
}

function BackendMessages({ errors }: { errors: string[] }) {
  if (errors.length === 0) return null;
  return (
    <div className="space-y-2 rounded-xl border border-amber-300/25 bg-amber-200/[0.06] p-4" role="status" aria-live="polite">
      <div className="flex items-start gap-3">
        <AlertIcon className="mt-0.5 shrink-0 text-amber-200" />
        <div className="min-w-0">
          <p className="text-sm font-semibold text-amber-100">The backend reported attention items</p>
          <p className="mt-1 text-xs leading-relaxed text-amber-100/65">No warning is hidden; a partial operation may have left devices in different states.</p>
          <ul className="mt-3 space-y-2">
            {errors.map((error, index) => {
              const copy = getErrorCopy(error);
              return (
                <li key={`${error}-${index}`} className="rounded-lg border border-amber-300/15 bg-background/20 px-3 py-2 text-xs">
                  <p className="font-semibold text-amber-100">{copy.title}</p>
                  <p className="mt-0.5 leading-relaxed text-amber-100/65">{copy.guidance}</p>
                  <p className="mt-1 break-words font-mono text-amber-100/45">{error}</p>
                </li>
              );
            })}
          </ul>
        </div>
      </div>
    </div>
  );
}

function FatalState({ unsupported, error, onRetry }: { unsupported: boolean; error: UiError | null; onRetry: () => void }) {
  return (
    <main className="relative flex min-h-screen items-center justify-center overflow-hidden px-5 py-10">
      <div className="dashboard-grid pointer-events-none absolute inset-0" />
      <Card className="panel-highlight relative w-full max-w-lg border-border/75 bg-card/90 p-2 shadow-[0_24px_80px_rgba(0,0,0,0.35)]">
        <CardHeader className="p-7 pb-4">
          <div className="mb-5 flex items-center gap-3">
            <ControwlyMark className="text-primary" />
            <div>
              <p className="eyebrow">Controwly</p>
              <p className="text-xs text-muted-foreground">Controller control</p>
            </div>
          </div>
          <div className="mb-3 flex h-11 w-11 items-center justify-center rounded-xl bg-secondary text-muted-foreground">
            {unsupported ? <MonitorIcon className="h-5 w-5" /> : <AlertIcon className="h-5 w-5 text-red-300" />}
          </div>
          <CardTitle>{unsupported ? "Open the Controwly desktop app" : "Controller backend unavailable"}</CardTitle>
          <CardDescription className="mt-2">
            {unsupported
              ? "This browser preview cannot enumerate or change controllers. Use the packaged desktop app so the native backend can identify real devices."
              : "Controwly could not get an inventory snapshot from its native backend. No device data has been invented or cached as a substitute."}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4 px-7 pb-7">
          {error ? <ErrorBanner error={error} /> : null}
          <div className="rounded-xl border border-border/70 bg-background/40 p-4 text-sm leading-relaxed text-muted-foreground">
            <p className="font-semibold text-foreground">What to try</p>
            <p className="mt-1">{unsupported ? "Launch Controwly from its installed desktop shortcut. Controller discovery requires native OS access." : "Confirm Controwly is running, then retry. If your OS asks for authorization, approve only a request you initiated."}</p>
          </div>
          {!unsupported ? (
            <Button type="button" className="w-full" onClick={onRetry}>
              <RefreshIcon className="h-4 w-4" />
              Try again
            </Button>
          ) : null}
        </CardContent>
      </Card>
    </main>
  );
}

function EmptyState({ onConnect }: { onConnect: () => void }) {
  return (
    <div className="flex flex-col items-center px-6 py-16 text-center">
      <div className="flex h-14 w-14 items-center justify-center rounded-2xl border border-border/80 bg-secondary/70 text-muted-foreground">
        <GamepadIcon className="h-7 w-7" />
      </div>
      <h3 className="mt-5 text-base font-semibold text-foreground">No controllers detected</h3>
      <p className="mt-2 max-w-md text-sm leading-relaxed text-muted-foreground">
        Connect a controller over USB or Bluetooth, then refresh. Discovery is based on controller capabilities, not a fixed vendor allowlist.
      </p>
      <Button type="button" variant="outline" className="mt-5" onClick={onConnect}>
        <BluetoothIcon className="h-4 w-4" />
        Connect a controller
      </Button>
    </div>
  );
}

function LoadingState() {
  return (
    <div className="space-y-3 px-5 py-6" aria-label="Loading controller inventory" aria-busy="true">
      {["loading-row-a", "loading-row-b", "loading-row-c"].map((key) => (
        <div key={key} className="flex items-center gap-4 rounded-xl border border-border/50 bg-background/25 p-4">
          <div className="h-4 w-4 animate-pulse rounded bg-secondary" />
          <div className="min-w-0 flex-1 space-y-2">
            <div className="h-4 w-40 animate-pulse rounded bg-secondary" />
            <div className="h-3 w-24 animate-pulse rounded bg-secondary/70" />
          </div>
          <div className="h-9 w-20 animate-pulse rounded-md bg-secondary" />
        </div>
      ))}
    </div>
  );
}

function ControllerRow({
  device,
  busy,
  onSelect,
  onToggleEnabled,
  onDetails,
}: {
  device: ControllerDevice;
  busy: boolean;
  onSelect: (id: string, selected: boolean) => void;
  onToggleEnabled: (id: string, enabled: boolean) => void;
  onDetails: (device: ControllerDevice) => void;
}) {
  const hasProblem = device.problemCode !== null && device.problemCode !== undefined;
  const connection = device.connection.trim();
  return (
    <div className={cn("group grid gap-4 border-b border-border/60 px-5 py-4 transition-colors last:border-b-0 hover:bg-accent/30 sm:grid-cols-[auto_minmax(0,1fr)_auto] sm:items-center", busy && "opacity-75")}>
      <Checkbox
        checked={device.selected}
        disabled={busy}
        onCheckedChange={(checked) => onSelect(device.id, checked === true)}
        aria-label={`Select ${device.name}`}
        className="mt-1 sm:mt-0"
      />
      <div className="min-w-0 pl-8 sm:pl-0">
        <div className="flex flex-wrap items-center gap-2">
          <p className="truncate text-sm font-semibold text-foreground">{device.name}</p>
          <Badge variant="outline" className="border-border/80 bg-background/30 text-[0.65rem] font-medium text-muted-foreground">{connection}</Badge>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
          <span className={cn("inline-flex items-center gap-1.5 font-medium", device.enabled ? "text-primary" : "text-amber-200")}>
            <span className={cn("status-dot h-1.5 w-1.5", device.enabled ? "bg-primary" : "bg-amber-300")} />
            {device.enabled ? "Enabled" : "Disabled"}
          </span>
          {hasProblem ? <span className="text-amber-200/75">Needs attention · code {device.problemCode}</span> : null}
          <span className="text-muted-foreground/70">{device.selected ? "Included in bulk actions" : "Not selected"}</span>
        </div>
      </div>
      <div className="flex items-center justify-end gap-2 pl-8 sm:pl-0">
        <Button
          type="button"
          variant={device.enabled ? "outline" : "default"}
          size="sm"
          disabled={busy}
          onClick={() => onToggleEnabled(device.id, !device.enabled)}
          aria-label={`${device.enabled ? "Disable" : "Enable"} ${device.name}`}
        >
          {device.enabled ? "Disable" : "Enable"}
        </Button>
        <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => onDetails(device)}>
          Details
        </Button>
      </div>
    </div>
  );
}

function ConnectionDialog({
  open,
  onOpenChange,
  onOpenBluetooth,
  busy,
  notice,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onOpenBluetooth: () => void;
  busy: boolean;
  notice: string | null;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <div className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-primary/10 text-primary">
            <BluetoothIcon className="h-5 w-5" />
          </div>
          <DialogTitle>Connect a controller</DialogTitle>
          <DialogDescription>
            Open your operating system’s Bluetooth manager and pair the controller there. Controwly refreshes the inventory automatically when you return.
          </DialogDescription>
        </DialogHeader>

        <Button type="button" className="w-full" onClick={onOpenBluetooth} disabled={busy}>
          {busy ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <BluetoothIcon className="h-4 w-4" />}
          {busy ? "Opening Bluetooth settings…" : "Open Bluetooth settings"}
        </Button>
        {notice ? (
          <div className="flex items-center gap-2 rounded-lg border border-primary/20 bg-primary/[0.07] px-3 py-2 text-xs text-primary" role="status">
            <CheckCircleIcon className="h-4 w-4 shrink-0" />
            {notice}
          </div>
        ) : null}

        <Separator />
        <div>
          <p className="eyebrow">Pairing quick guide</p>
          <ol className="mt-3 space-y-3">
            {PAIRING_GUIDES.map((guide, index) => (
              <li key={guide.title} className="flex gap-3">
                <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-secondary text-xs font-semibold text-muted-foreground">{index + 1}</span>
                <div>
                  <p className="text-sm font-semibold text-foreground">{guide.title}</p>
                  <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{guide.detail}</p>
                </div>
              </li>
            ))}
          </ol>
        </div>
        <p className="rounded-lg border border-border/70 bg-background/35 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
          USB controllers usually appear after they are plugged in. A controller can be visible to Bluetooth while still unavailable to Controwly if its capabilities are not supported by the native backend.
        </p>
        <DialogFooter>
          <DialogClose asChild>
            <Button type="button" variant="outline" className="w-full sm:w-auto">Done</Button>
          </DialogClose>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function DeviceDetailsDialog({
  device,
  onOpenChange,
  onCopy,
  copied,
}: {
  device: ControllerDevice | null;
  onOpenChange: (open: boolean) => void;
  onCopy: (id: string) => void;
  copied: boolean;
}) {
  return (
    <Dialog open={device !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        {device ? (
          <>
            <DialogHeader>
              <div className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-secondary text-muted-foreground">
                <GamepadIcon className="h-5 w-5" />
              </div>
              <DialogTitle>{device.name}</DialogTitle>
              <DialogDescription>Device identity and current native backend state.</DialogDescription>
            </DialogHeader>
            <dl className="divide-y divide-border/60 rounded-xl border border-border/70 bg-background/30">
              <div className="grid gap-1 px-4 py-3 sm:grid-cols-[7rem_1fr] sm:items-center">
                <dt className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Connection</dt>
                <dd className="text-sm text-foreground">{device.connection}</dd>
              </div>
              <div className="grid gap-1 px-4 py-3 sm:grid-cols-[7rem_1fr] sm:items-center">
                <dt className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">State</dt>
                <dd className="text-sm text-foreground">{device.enabled ? "Enabled" : "Disabled"}</dd>
              </div>
              <div className="grid gap-1 px-4 py-3 sm:grid-cols-[7rem_1fr] sm:items-start">
                <dt className="pt-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">Device ID</dt>
                <dd className="flex min-w-0 items-start gap-2">
                  <code className="min-w-0 flex-1 break-all rounded-md bg-secondary/70 px-2 py-1.5 text-xs leading-relaxed text-foreground">{device.id}</code>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button type="button" variant="outline" size="icon" className="h-8 w-8 shrink-0" onClick={() => onCopy(device.id)} aria-label="Copy device ID">
                        {copied ? <CheckIcon className="h-4 w-4 text-primary" /> : <CopyIcon className="h-4 w-4" />}
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>{copied ? "Copied" : "Copy device ID"}</TooltipContent>
                  </Tooltip>
                </dd>
              </div>
              {device.problemCode !== null && device.problemCode !== undefined ? (
                <div className="grid gap-1 px-4 py-3 sm:grid-cols-[7rem_1fr] sm:items-center">
                  <dt className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Problem code</dt>
                  <dd className="text-sm text-amber-200">{device.problemCode}</dd>
                </div>
              ) : null}
            </dl>
            <DialogFooter>
              <DialogClose asChild>
                <Button type="button" variant="outline" className="w-full sm:w-auto">Close</Button>
              </DialogClose>
            </DialogFooter>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function QuitDialog({
  open,
  onOpenChange,
  onRestoreAndQuit,
  onQuitKeepState,
  pending,
  error,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRestoreAndQuit: () => void;
  onQuitKeepState: () => void;
  pending: PendingOperation | null;
  error: UiError | null;
}) {
  const restoreBusy = pending?.kind === "restore-and-quit";
  const keepStateBusy = pending?.kind === "quit-keep-state";
  const busy = Boolean(pending);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <div className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-amber-300/10 text-amber-200">
            <ShieldIcon className="h-5 w-5" />
          </div>
          <DialogTitle>Before you quit</DialogTitle>
          <DialogDescription>
            Choose how Controwly should leave controller state. Closing the window normally hides it to the tray instead.
          </DialogDescription>
        </DialogHeader>
        <div className="rounded-xl border border-amber-300/20 bg-amber-300/[0.05] p-4 text-sm leading-relaxed text-amber-100/80">
          <p className="font-semibold text-amber-100">Recommended: restore, then quit</p>
          <p className="mt-1 text-xs">Controwly will ask the native backend to restore only devices it previously disabled, then exit. Devices disabled before Controwly acted stay disabled.</p>
        </div>
        <p className="rounded-lg border border-red-300/15 bg-red-300/[0.04] px-3 py-2 text-xs leading-relaxed text-red-100/75">Choosing “Quit keeping current state” leaves controllers disabled by Controwly in that state after exit. Reopen Controwly to restore them.</p>
        {error ? <ErrorBanner error={error} /> : null}
        <DialogFooter className="gap-2 sm:flex-row sm:items-center sm:justify-end">
          <DialogClose asChild>
            <Button type="button" variant="ghost" disabled={busy}>Cancel</Button>
          </DialogClose>
          <Button type="button" variant="outline" onClick={onQuitKeepState} disabled={busy}>
            {keepStateBusy ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <LockIcon className="h-4 w-4" />}
            Quit keeping current state
          </Button>
          <Button type="button" onClick={onRestoreAndQuit} disabled={busy}>
            {restoreBusy ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <ShieldIcon className="h-4 w-4" />}
            Restore controllers and quit
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function UpdatePanel({
  state,
  serviceError,
  pending,
  onCheck,
  onInstall,
}: {
  state: UpdateState | null;
  serviceError: string | null;
  pending: PendingOperation | null;
  onCheck: () => void;
  onInstall: () => void;
}) {
  const status = state ? UPDATE_COPY[state.status] : null;
  const progress = state?.total ? Math.min(100, Math.max(0, (state.downloaded / state.total) * 100)) : 0;
  const isChecking = pending?.kind === "update-check";
  const isInstalling = pending?.kind === "update-install";
  return (
    <Card className="border-border/70 bg-card/70">
      <CardHeader className="p-5 pb-3">
        <div className="flex items-start justify-between gap-3">
          <div>
            <p className="eyebrow">About & updates</p>
            <CardTitle className="mt-2 text-base">Keep Controwly current</CardTitle>
          </div>
          <Badge variant="outline" className="border-border/80 text-[0.65rem] text-muted-foreground">Manual</Badge>
        </div>
        <CardDescription className="pt-1">Updates never install silently. Check when you choose, then install an available release explicitly.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 px-5 pb-4">
        {state && status ? (
          <div className="rounded-lg border border-border/70 bg-background/35 p-3">
            <div className="flex items-center justify-between gap-3">
              <p className={cn("text-sm font-semibold", state.status === "error" ? "text-red-200" : state.status === "available" ? "text-primary" : "text-foreground")}>{status.label}</p>
              {state.version ? <span className="font-mono text-xs text-muted-foreground">v{state.version}</span> : null}
            </div>
            <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{status.detail}</p>
            {state.status === "available" ? (
              <Button type="button" size="sm" className="mt-3 w-full" onClick={onInstall} disabled={Boolean(pending)}>
                {isInstalling ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <CheckCircleIcon className="h-4 w-4" />}
                {isInstalling ? "Starting installation…" : `Install v${state.version ?? "new"}`}
              </Button>
            ) : null}
            {state.status === "downloading" || state.status === "installing" ? (
              <div className="mt-3 space-y-1.5" aria-live="polite">
                <div className="flex justify-between text-[0.68rem] text-muted-foreground">
                  <span>{state.status === "installing" ? "Preparing installation" : "Download progress"}</span>
                  <span>{state.total ? `${formatBytes(state.downloaded)} / ${formatBytes(state.total)}` : formatBytes(state.downloaded)}</span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-secondary" role="progressbar" aria-valuemin={0} aria-valuemax={state.total ?? undefined} aria-valuenow={state.total ? state.downloaded : undefined} aria-label="Update progress">
                  <div className="h-full rounded-full bg-primary transition-[width]" style={{ width: `${state.total ? progress : state.status === "installing" ? 100 : 12}%` }} />
                </div>
              </div>
            ) : null}
            {state.error ? <p className="mt-2 break-words text-xs text-red-200">{state.error}</p> : null}
          </div>
        ) : (
          <div className="rounded-lg border border-border/70 bg-background/35 p-3 text-xs leading-relaxed text-muted-foreground">
            The update service has not reported a status yet. A manual check will ask the native updater for one.
          </div>
        )}
        {serviceError ? <p className="text-xs leading-relaxed text-amber-200/80">{serviceError}</p> : null}
      </CardContent>
      <CardFooter className="px-5 pb-5">
        <Button type="button" variant="outline" size="sm" className="w-full" onClick={onCheck} disabled={Boolean(pending)}>
          {isChecking ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <RefreshIcon className="h-4 w-4" />}
          {isChecking ? "Checking…" : "Check for updates"}
        </Button>
      </CardFooter>
    </Card>
  );
}

export function ControllerDashboard() {
  const [phase, setPhase] = useState<DashboardPhase>("loading");
  const [controllerState, setControllerState] = useState<ControllerState | null>(null);
  const [updateState, setUpdateState] = useState<UpdateState | null>(null);
  const [connectionError, setConnectionError] = useState<UiError | null>(null);
  const [actionError, setActionError] = useState<UiError | null>(null);
  const [backgroundError, setBackgroundError] = useState<UiError | null>(null);
  const [liveEventsError, setLiveEventsError] = useState<string | null>(null);
  const [quitEventsError, setQuitEventsError] = useState<string | null>(null);
  const [updateServiceError, setUpdateServiceError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingOperation | null>(null);
  const [connectOpen, setConnectOpen] = useState(false);
  const [quitOpen, setQuitOpen] = useState(false);
  const [quitError, setQuitError] = useState<UiError | null>(null);
  const [bluetoothNotice, setBluetoothNotice] = useState<string | null>(null);
  const [detailDeviceId, setDetailDeviceId] = useState<string | null>(null);
  const [copiedDeviceId, setCopiedDeviceId] = useState<string | null>(null);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const requestInFlightRef = useRef(false);
  const controllerStateRef = useRef<ControllerState | null>(null);
  controllerStateRef.current = controllerState;

  const runOperation = useCallback(
    async function execute<T>(
      operation: PendingOperation,
      task: () => Promise<T>,
      onSuccess: (result: T) => void,
      onFailure?: (error: UiError) => void,
    ): Promise<void> {
      if (requestInFlightRef.current || !isTauriRuntime()) return;
      requestInFlightRef.current = true;
      setPending(operation);
      try {
        const result = await task();
        onSuccess(result);
        setActionError(null);
      } catch (error) {
        const uiError = { title: `${operation.label} failed`, detail: formatError(error) };
        setActionError(uiError);
        onFailure?.(uiError);
      } finally {
        requestInFlightRef.current = false;
        setPending(null);
      }
    },
    [],
  );

  useEffect(() => {
    let disposed = false;
    let controllerUnlisten: (() => void) | undefined;
    let updaterUnlisten: (() => void) | undefined;
    let closeUnlisten: (() => void) | undefined;

    const bootstrap = async () => {
      if (!isTauriRuntime()) {
        setPhase("unsupported");
        return;
      }

      requestInFlightRef.current = true;
      try {
        try {
          const listener = await controllerApi.onStateChange((nextState) => {
            if (disposed) return;
            setControllerState(nextState);
            setPhase((current) => current === "error" ? "ready" : current);
            setConnectionError(null);
            setLastUpdated(new Date());
          });
          if (disposed) listener();
          else controllerUnlisten = listener;
        } catch (error) {
          if (!disposed) setLiveEventsError(`Live controller updates are unavailable: ${formatError(error)}`);
        }

        try {
          const listener = await controllerApi.onUpdateStateChange((nextState) => {
            if (disposed) return;
            setUpdateState(nextState);
            setUpdateServiceError(null);
          });
          if (disposed) listener();
          else updaterUnlisten = listener;
        } catch (error) {
          if (!disposed) setUpdateServiceError(`Live update status is unavailable: ${formatError(error)}`);
        }

        try {
          const listener = await controllerApi.onCloseRequested(() => {
            if (disposed) return;
            setQuitOpen(true);
          });
          if (disposed) listener();
          else closeUnlisten = listener;
        } catch (error) {
          if (!disposed) setQuitEventsError(`Window close handling is unavailable: ${formatError(error)}`);
        }

        const nextControllerState = await controllerApi.getState();
        if (disposed) return;
        setControllerState(nextControllerState);
        setConnectionError(null);
        setPhase("ready");
        setLastUpdated(new Date());

        try {
          const nextUpdateState = await controllerApi.getUpdateState();
          if (!disposed) {
            setUpdateState(nextUpdateState);
            setUpdateServiceError(null);
          }
        } catch (error) {
          if (!disposed) setUpdateServiceError(`Update status is unavailable: ${formatError(error)}`);
        }
      } catch (error) {
        if (!disposed) {
          const detail = formatError(error);
          setConnectionError({ title: "Could not connect to the controller backend", detail });
          setPhase(isTauriRuntime() ? "error" : "unsupported");
        }
      } finally {
        requestInFlightRef.current = false;
      }
    };

    void bootstrap();
    return () => {
      disposed = true;
      controllerUnlisten?.();
      updaterUnlisten?.();
      closeUnlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime() || phase === "unsupported") return;
    let disposed = false;

    const poll = async () => {
      if (disposed || document.visibilityState !== "visible" || requestInFlightRef.current) return;
      requestInFlightRef.current = true;
      try {
        const nextState = await controllerApi.getState();
        if (!disposed) {
          setControllerState(nextState);
          setPhase((current) => current === "error" ? "ready" : current);
          setBackgroundError(null);
          setLastUpdated(new Date());
        }
      } catch (error) {
        if (!disposed) setBackgroundError({ title: "Live inventory refresh failed", detail: formatError(error) });
      } finally {
        requestInFlightRef.current = false;
      }
    };

    const timer = window.setInterval(() => void poll(), 3000);
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") void poll();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      disposed = true;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [phase]);

  const devices = controllerState?.devices ?? [];
  const enabledCount = devices.filter((device) => device.enabled).length;
  const disabledCount = devices.length - enabledCount;
  const selectedCount = devices.filter((device) => device.selected).length;
  const selectedLabel = selectedCount === 1 ? "1 controller selected" : `${selectedCount} controllers selected`;
  const platform = useMemo(getPlatform, []);
  const authorizationGuidance = useMemo(() => getAuthorizationGuidance(platform), [platform]);
  const detailDevice = detailDeviceId ? devices.find((device) => device.id === detailDeviceId) ?? null : null;
  const shortcutAvailable = controllerState !== null && controllerState.shortcutAvailable && controllerState.shortcut.trim().length > 0;
  const hasWarnings = Boolean(controllerState?.errors.length || (controllerState && !shortcutAvailable) || connectionError || actionError || backgroundError || liveEventsError || quitEventsError || quitError);
  const busy = Boolean(pending);
  const handleRefresh = () => {
    void runOperation(
      { kind: "refresh", label: "Refresh" },
      () => controllerApi.getState(),
      (nextState) => {
        setControllerState(nextState);
        setConnectionError(null);
        setBackgroundError(null);
        setPhase("ready");
        setLastUpdated(new Date());
      },
    );
  };

  const handleSelect = (id: string, selected: boolean) => {
    const device = devices.find((candidate) => candidate.id === id);
    void runOperation(
      { kind: "selection", id, selected, label: selected ? `Select ${device?.name ?? "controller"}` : `Deselect ${device?.name ?? "controller"}` },
      () => controllerApi.setSelected(id, selected),
      (nextState) => {
        setControllerState(nextState);
        setLastUpdated(new Date());
      },
    );
  };

  const handleToggleEnabled = (id: string, enabled: boolean) => {
    const device = devices.find((candidate) => candidate.id === id);
    void runOperation(
      { kind: "device", id, enabled, label: `${enabled ? "Enable" : "Disable"} ${device?.name ?? "controller"}` },
      () => controllerApi.setDeviceEnabled(id, enabled),
      (nextState) => {
        setControllerState(nextState);
        setLastUpdated(new Date());
      },
    );
  };

  const handleBulk = (enabled: boolean) => {
    void runOperation(
      { kind: "bulk", enabled, label: `${enabled ? "Enable" : "Disable"} selected controllers` },
      () => controllerApi.setSelectedEnabled(enabled),
      (nextState) => {
        setControllerState(nextState);
        setLastUpdated(new Date());
      },
    );
  };

  const handleRestore = () => {
    void runOperation(
      { kind: "restore", label: "Restore Controwly-disabled devices" },
      () => controllerApi.restoreDisabled(),
      (nextState) => {
        setControllerState(nextState);
        setLastUpdated(new Date());
      },
    );
  };

  const handleOpenBluetooth = () => {
    void runOperation(
      { kind: "bluetooth", label: "Open Bluetooth settings" },
      () => controllerApi.openBluetoothSettings(),
      () => setBluetoothNotice("Bluetooth settings requested. If no window appears, open your OS Bluetooth manager manually. Controwly refreshes automatically after pairing."),
    );
  };

  const handleCheckUpdates = () => {
    void runOperation(
      { kind: "update-check", label: "Check for updates" },
      () => controllerApi.checkForUpdates(),
      (nextState) => {
        setUpdateState(nextState);
        setUpdateServiceError(null);
      },
    );
  };

  const handleInstallUpdate = () => {
    if (updateState?.status !== "available") return;
    void runOperation(
      { kind: "update-install", label: "Install update" },
      () => controllerApi.installUpdate(),
      () => setUpdateServiceError("Installation started. Follow the native updater prompt; Controwly may restart."),
    );
  };

  const handleRestoreAndQuit = () => {
    void runOperation(
      { kind: "restore-and-quit", label: "Restore controllers and quit" },
      () => controllerApi.restoreAndQuit(),
      () => setQuitError(null),
      (error) => setQuitError(error),
    );
  };

  const handleQuitKeepState = () => {
    void runOperation(
      { kind: "quit-keep-state", label: "Quit keeping current state" },
      () => controllerApi.quitKeepState(),
      () => setQuitError(null),
      (error) => setQuitError(error),
    );
  };

  const handleCopyDeviceId = async (id: string) => {
    try {
      if (!navigator.clipboard) throw new Error("Clipboard access is unavailable in this window.");
      await navigator.clipboard.writeText(id);
      setCopiedDeviceId(id);
      window.setTimeout(() => setCopiedDeviceId((current) => current === id ? null : current), 1800);
    } catch (error) {
      setActionError({ title: "Could not copy device ID", detail: formatError(error) });
    }
  };

  if (phase === "unsupported") {
    return <FatalState unsupported error={null} onRetry={handleRefresh} />;
  }
  if (phase === "error" && !controllerState) {
    return <FatalState unsupported={false} error={connectionError} onRetry={handleRefresh} />;
  }

  const shortcut = controllerState?.shortcut ?? "";
  return (
    <TooltipProvider delayDuration={350}>
      <div className="relative min-h-screen overflow-hidden bg-background">
        <div className="dashboard-grid pointer-events-none absolute inset-0" />
        <div className="pointer-events-none absolute -left-40 -top-40 h-[30rem] w-[30rem] rounded-full bg-primary/[0.045] blur-3xl" />
        <div className="relative mx-auto max-w-7xl px-4 pb-10 sm:px-6 lg:px-8">
          <header className="flex flex-col gap-6 border-b border-border/60 py-6 sm:flex-row sm:items-center sm:justify-between sm:py-8">
            <div className="flex items-center gap-3">
              <ControwlyMark className="shrink-0 text-primary" />
              <div>
                <p className="eyebrow">Controwly / device control</p>
                <h1 className="mt-1 text-xl font-semibold tracking-tight text-foreground sm:text-2xl">Controller dashboard</h1>
                <p className="mt-1 max-w-xl text-sm text-muted-foreground">Manage real controllers identified by the native backend.</p>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2 sm:justify-end">
              <StatusPill phase={phase} hasWarnings={hasWarnings} />
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button type="button" variant="outline" size="icon" onClick={handleRefresh} disabled={busy || phase === "loading"} aria-label="Refresh controller inventory">
                    {pending?.kind === "refresh" ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <RefreshIcon className="h-4 w-4" />}
                  </Button>
                </TooltipTrigger>
                <TooltipContent>Refresh inventory</TooltipContent>
              </Tooltip>
              <Button type="button" variant="outline" size="sm" onClick={() => { setBluetoothNotice(null); setConnectOpen(true); }} disabled={busy}>
                <BluetoothIcon className="h-4 w-4" />
                Connect
              </Button>
            </div>
          </header>

          <main className="pt-6 sm:pt-8">
            <div className="space-y-3">
              {connectionError ? <ErrorBanner error={connectionError} onDismiss={() => setConnectionError(null)} /> : null}
              {actionError ? <ErrorBanner error={actionError} onDismiss={() => setActionError(null)} /> : null}
              {backgroundError ? <ErrorBanner error={backgroundError} onDismiss={() => setBackgroundError(null)} /> : null}
              {liveEventsError ? <ErrorBanner error={{ title: "Live updates unavailable", detail: liveEventsError }} /> : null}
              {quitEventsError ? <ErrorBanner error={{ title: "Window close handling unavailable", detail: quitEventsError }} /> : null}
              {controllerState ? <BackendMessages errors={controllerState.errors} /> : null}
            </div>

            <section className="mt-5 grid gap-3 sm:grid-cols-3" aria-label="Controller counts">
              <MetricCard label="Enabled" value={enabledCount} detail="Ready for system-wide use" tone="primary" />
              <MetricCard label="Disabled" value={disabledCount} detail="Review before restoring" tone="amber" />
              <MetricCard label="Selected" value={selectedCount} detail="Included in bulk actions" tone="violet" />
            </section>

            <section className="mt-4 grid gap-4 lg:grid-cols-[minmax(0,1fr)_20rem] lg:items-start">
              <div className="min-w-0 space-y-4">
                <Card className="panel-highlight border-border/70 bg-card/85">
                  <CardHeader className="flex-row items-start justify-between gap-4 p-5 pb-4 sm:p-6 sm:pb-4">
                    <div>
                      <CardTitle className="text-base">Selected controllers</CardTitle>
                      <CardDescription className="mt-1">Apply one state to every selected device. Each request is serialized through the native backend.</CardDescription>
                    </div>
                    <Badge variant={selectedCount > 0 ? "default" : "outline"} className="shrink-0 text-[0.65rem]">{selectedCount} selected</Badge>
                  </CardHeader>
                  <CardContent className="p-5 pt-0 sm:p-6 sm:pt-0">
                    <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
                      <Button type="button" size="sm" onClick={() => handleBulk(true)} disabled={busy || selectedCount === 0}>
                        {pending?.kind === "bulk" && pending.enabled ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <CheckIcon className="h-4 w-4" />}
                        Enable selected
                      </Button>
                      <Button type="button" variant="outline" size="sm" onClick={() => handleBulk(false)} disabled={busy || selectedCount === 0}>
                        {pending?.kind === "bulk" && !pending.enabled ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <LockIcon className="h-4 w-4" />}
                        Disable selected
                      </Button>
                      <p className="text-xs text-muted-foreground sm:ml-auto">{selectedCount > 0 ? selectedLabel : "Select devices below to enable bulk actions."}</p>
                    </div>
                  </CardContent>
                </Card>

                <Card className="border-border/70 bg-card/85">
                  <CardHeader className="flex-row items-center justify-between gap-4 p-5 pb-4 sm:p-6 sm:pb-4">
                    <div>
                      <CardTitle className="text-base">Controller inventory</CardTitle>
                      <CardDescription className="mt-1">USB and Bluetooth devices appear here when their capabilities are available.</CardDescription>
                    </div>
                    {controllerState ? <span className="text-xs font-medium text-muted-foreground">{devices.length} {devices.length === 1 ? "device" : "devices"}</span> : null}
                  </CardHeader>
                  <CardContent className="p-0">
                    {phase === "loading" && !controllerState ? <LoadingState /> : devices.length === 0 ? <EmptyState onConnect={() => { setBluetoothNotice(null); setConnectOpen(true); }} /> : (
                      <div role="list" aria-label="Detected controllers" aria-busy={busy}>
                        {devices.map((device) => (
                          <div role="listitem" key={device.id}>
                            <ControllerRow
                              device={device}
                              busy={busy}
                              onSelect={handleSelect}
                              onToggleEnabled={handleToggleEnabled}
                              onDetails={(nextDevice) => { setCopiedDeviceId(null); setDetailDeviceId(nextDevice.id); }}
                            />
                          </div>
                        ))}
                      </div>
                    )}
                  </CardContent>
                  {controllerState ? <CardFooter className="justify-between border-t border-border/60 px-5 py-3 sm:px-6">
                    <span className="text-[0.68rem] text-muted-foreground">{formatUpdatedAt(lastUpdated)} · refreshes while this window is visible</span>
                    {pending ? <span className="inline-flex items-center gap-1.5 text-[0.68rem] text-primary"><LoaderIcon className="h-3.5 w-3.5 animate-spin" />{pending.label}</span> : null}
                  </CardFooter> : null}
                </Card>
              </div>

              <aside className="space-y-4" aria-label="Controller guidance">
                <Card className="border-primary/20 bg-primary/[0.045]">
                  <CardHeader className="p-5 pb-3">
                    <div className="flex items-start gap-3">
                      <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary"><ShieldIcon className="h-4.5 w-4.5" /></div>
                      <div>
                        <CardTitle className="text-base">System-wide control</CardTitle>
                        <CardDescription className="mt-1">Disabling is applied by the operating system, not just this window.</CardDescription>
                      </div>
                    </div>
                  </CardHeader>
                  <CardContent className="space-y-3 px-5 pb-5 text-xs leading-relaxed text-muted-foreground">
                    <p>{authorizationGuidance}</p>
                    <p>Restore only returns devices that Controwly previously disabled. It will not automatically enable a device that was already disabled before Controwly acted.</p>
                    <Button type="button" variant="outline" size="sm" className="w-full" onClick={handleRestore} disabled={busy}>
                      {pending?.kind === "restore" ? <LoaderIcon className="h-4 w-4 animate-spin" /> : <RefreshIcon className="h-4 w-4" />}
                      Restore Controwly-disabled devices
                    </Button>
                  </CardContent>
                </Card>

                <Card className="border-border/70 bg-card/70">
                  <CardHeader className="p-5 pb-3">
                    <div className={cn("flex items-center gap-2", shortcutAvailable ? "text-primary" : "text-amber-200")}><KeyboardIcon className="h-4 w-4" /><p className={cn("eyebrow", shortcutAvailable ? "text-primary/80" : "text-amber-200/80")}>Keyboard shortcut</p></div>
                    <CardTitle className="mt-2 text-base">Toggle selected devices</CardTitle>
                    <CardDescription className="mt-1">{shortcutAvailable ? "Use the global shortcut from anywhere while Controwly is running." : "Unavailable on this desktop. The native backend did not confirm global shortcut registration."}</CardDescription>
                  </CardHeader>
                  <CardContent className="px-5 pb-5">
                    {shortcutAvailable ? (
                      <div className="flex items-center gap-2">
                        {shortcut.split("+").map((part) => <kbd key={part} className="shortcut-key">{part.trim()}</kbd>)}
                        <span className="ml-1 text-xs text-muted-foreground">toggles selected</span>
                      </div>
                    ) : (
                      <div className="flex items-start gap-2 rounded-lg border border-amber-300/20 bg-amber-300/[0.05] px-3 py-2 text-xs leading-relaxed text-amber-100/75" role="status">
                        <AlertIcon className="mt-0.5 h-4 w-4 shrink-0 text-amber-200" />
                        <span>Unavailable on this desktop. Use the enable and disable controls below. Check backend warnings for the registration reason.</span>
                      </div>
                    )}
                  </CardContent>
                </Card>

                <UpdatePanel
                  state={updateState}
                  serviceError={updateServiceError}
                  pending={pending}
                  onCheck={handleCheckUpdates}
                  onInstall={handleInstallUpdate}
                />

                <div className="rounded-xl border border-border/60 bg-card/35 p-4 text-xs leading-relaxed text-muted-foreground">
                  <div className="flex items-start gap-2"><InfoIcon className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" /><p>Close hides Controwly to the tray. Use the tray menu to show it again or quit explicitly.</p></div>
                </div>
              </aside>
            </section>
          </main>

          <footer className="mt-8 flex flex-col gap-3 border-t border-border/50 pt-5 text-[0.68rem] text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
            <span>Controwly acts only on devices identified by the native backend.</span>
            <div className="flex flex-wrap items-center gap-3 sm:justify-end">
              <span>{formatUpdatedAt(lastUpdated)}</span>
              <Button type="button" variant="ghost" size="sm" className="h-8 text-xs text-muted-foreground hover:text-foreground" onClick={() => { setQuitError(null); setQuitOpen(true); }} disabled={busy}>
                <XIcon className="h-3.5 w-3.5" />
                Quit Controwly
              </Button>
            </div>
          </footer>
        </div>
      </div>
      <ConnectionDialog
        open={connectOpen}
        onOpenChange={setConnectOpen}
        onOpenBluetooth={handleOpenBluetooth}
        busy={pending?.kind === "bluetooth"}
        notice={bluetoothNotice}
      />
      <DeviceDetailsDialog
        device={detailDevice}
        onOpenChange={(open) => { if (!open) setDetailDeviceId(null); }}
        onCopy={(id) => void handleCopyDeviceId(id)}
        copied={Boolean(detailDevice && copiedDeviceId === detailDevice.id)}
      />
      <QuitDialog
        open={quitOpen}
        onOpenChange={setQuitOpen}
        onRestoreAndQuit={handleRestoreAndQuit}
        onQuitKeepState={handleQuitKeepState}
        pending={pending}
        error={quitError}
      />
    </TooltipProvider>
  );
}
