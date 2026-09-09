import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ControllerDevice, ControllerState, UpdateState, UpdateStatus } from "@/types";

const UPDATE_STATUSES: readonly UpdateStatus[] = [
  "idle",
  "checking",
  "current",
  "available",
  "downloading",
  "installing",
  "error",
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function normaliseDevice(value: unknown): ControllerDevice | null {
  if (!isRecord(value)) return null;

  const id = value.id;
  const name = value.name;
  const connection = value.connection;
  const enabled = value.enabled;
  const selected = value.selected;
  const problemCode = value.problemCode;

  if (
    !nonEmptyString(id) ||
    !nonEmptyString(name) ||
    !nonEmptyString(connection) ||
    typeof enabled !== "boolean" ||
    typeof selected !== "boolean"
  ) {
    return null;
  }

  return {
    id,
    name,
    connection,
    enabled,
    selected,
    problemCode:
      typeof problemCode === "number" && Number.isFinite(problemCode) && problemCode !== 0
        ? problemCode
        : null,
  };
}

export function normaliseState(value: unknown): ControllerState {
  if (!isRecord(value)) {
    return {
      devices: [],
      shortcut: "",
      shortcutAvailable: false,
      errors: ["The controller backend returned an invalid state."],
    };
  }

  const suppliedDevices = value.devices;
  const hasDevices = Array.isArray(suppliedDevices);
  const rawDevices: unknown[] = hasDevices ? suppliedDevices : [];
  let malformedCount = 0;
  const devices: ControllerDevice[] = [];
  for (const device of rawDevices) {
    const normalised = normaliseDevice(device);
    if (normalised) devices.push(normalised);
    else malformedCount += 1;
  }
  const suppliedErrors = value.errors;
  const hasErrors = Array.isArray(suppliedErrors);
  const rawErrors: unknown[] = hasErrors ? suppliedErrors : [];
  const backendErrors = rawErrors.filter(nonEmptyString).map((error) => error.trim());
  const errors = [...backendErrors];

  if (!hasDevices) {
    errors.push("The controller backend returned no device inventory.");
  }
  if (!hasErrors) {
    errors.push("The controller backend returned no error list.");
  }
  if (malformedCount > 0) {
    errors.push(
      `${malformedCount} controller record${malformedCount === 1 ? "" : "s"} could not be read. Refresh to try again.`,
    );
  }

  const shortcut = nonEmptyString(value.shortcut) ? value.shortcut.trim() : "";
  const hasShortcutAvailable = typeof value.shortcutAvailable === "boolean";
  const shortcutAvailable = hasShortcutAvailable && value.shortcutAvailable === true && shortcut.length > 0;
  if (!hasShortcutAvailable) {
    errors.push("Global shortcut registration status was not provided; the shortcut is unavailable.");
  } else if (value.shortcutAvailable && shortcut.length === 0) {
    errors.push("Global shortcut registration was reported without a shortcut; it is unavailable.");
  }

  return {
    devices,
    shortcut,
    shortcutAvailable,
    errors: [...new Set(errors)],
  };
}

export function normaliseUpdateState(value: unknown): UpdateState {
  if (!isRecord(value)) {
    return {
      status: "error",
      version: null,
      downloaded: 0,
      total: null,
      error: "The update service returned an invalid state.",
    };
  }

  const candidateStatus = value.status;
  const status = UPDATE_STATUSES.includes(candidateStatus as UpdateStatus)
    ? (candidateStatus as UpdateStatus)
    : "error";
  const downloaded =
    typeof value.downloaded === "number" && Number.isFinite(value.downloaded)
      ? Math.max(0, value.downloaded)
      : 0;
  const total =
    typeof value.total === "number" && Number.isFinite(value.total) && value.total > 0
      ? value.total
      : null;
  const version = typeof value.version === "string" && value.version.trim() ? value.version.trim() : null;
  const error = typeof value.error === "string" && value.error.trim() ? value.error.trim() : null;

  return {
    status,
    version,
    downloaded,
    total,
    error: status === "error" && !error ? "The update service reported an unknown error." : error,
  };
}

async function invokeUpdateState(command: string): Promise<UpdateState> {
  const result = await invoke<unknown>(command);
  return normaliseUpdateState(result);
}

export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  return (
    Object.prototype.hasOwnProperty.call(window, "__TAURI_INTERNALS__") ||
    Object.prototype.hasOwnProperty.call(window, "__TAURI__")
  );
}

async function invokeState(
  command: string,
  args?: Record<string, unknown>,
): Promise<ControllerState> {
  const result = await invoke<unknown>(command, args);
  return normaliseState(result);
}

export const controllerApi = {
  getState() {
    return invokeState("get_state");
  },

  setSelected(id: string, selected: boolean) {
    return invokeState("set_selected", { id, selected });
  },

  setDeviceEnabled(id: string, enabled: boolean) {
    return invokeState("set_device_enabled", { id, enabled });
  },

  setSelectedEnabled(enabled: boolean) {
    return invokeState("set_selected_enabled", { enabled });
  },

  restoreDisabled() {
    return invokeState("restore_disabled");
  },

  async openBluetoothSettings(): Promise<void> {
    await invoke<void>("open_bluetooth_settings");
  },

  async restoreAndQuit(): Promise<void> {
    await invoke<void>("restore_and_quit");
  },

  async quitKeepState(): Promise<void> {
    await invoke<void>("quit_keep_state");
  },

  getUpdateState() {
    return invokeUpdateState("get_update_state");
  },

  checkForUpdates() {
    return invokeUpdateState("check_for_updates");
  },

  async installUpdate(): Promise<void> {
    await invoke<void>("install_update");
  },

  onStateChange(callback: (state: ControllerState) => void): Promise<UnlistenFn> {
    return listen<unknown>("controller-state", (event) => {
      callback(normaliseState(event.payload));
    });
  },

  onUpdateStateChange(callback: (state: UpdateState) => void): Promise<UnlistenFn> {
    return listen<unknown>("updater-state", (event) => {
      callback(normaliseUpdateState(event.payload));
    });
  },

  onCloseRequested(callback: () => void): Promise<UnlistenFn> {
    return listen<null>("close-requested", () => {
      callback();
    });
  },
};

export function formatError(error: unknown): string {
  if (typeof error === "string" && error.trim()) return error.trim();
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  return "The controller backend did not complete that request.";
}
