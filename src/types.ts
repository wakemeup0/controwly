export type ControllerDevice = {
  id: string;
  name: string;
  connection: string;
  enabled: boolean;
  selected: boolean;
  problemCode?: number | null;
};

export type ControllerState = {
  devices: ControllerDevice[];
  shortcut: string;
  shortcutAvailable: boolean;
  errors: string[];
};

export type DashboardPhase = "loading" | "ready" | "unsupported" | "error";

export type PendingOperation =
  | { kind: "refresh"; label: string }
  | { kind: "selection"; id: string; selected: boolean; label: string }
  | { kind: "device"; id: string; enabled: boolean; label: string }
  | { kind: "bulk"; enabled: boolean; label: string }
  | { kind: "restore"; label: string }
  | { kind: "bluetooth"; label: string }
  | { kind: "update-check"; label: string }
  | { kind: "update-install"; label: string }
  | { kind: "restore-and-quit"; label: string }
  | { kind: "quit-keep-state"; label: string };

export type UpdateStatus =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "downloading"
  | "installing"
  | "error";

export type UpdateState = {
  status: UpdateStatus;
  version: string | null;
  downloaded: number;
  total: number | null;
  error: string | null;
};
