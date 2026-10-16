// Typed access to the Rust backend. All system work happens in Rust; this
// module only invokes narrowly scoped commands and listens to two events.
import type {
  AppInfo,
  CommandError,
  Config,
  ConfigEnvelope,
  ExternalLink,
  ImportPreview,
  PreviewRequest,
  PreviewResult,
  RuntimeView,
  StartupStatus,
} from "./types";

export type Unlisten = () => void;

export interface Backend {
  readonly kind: "tauri" | "preview";
  getAppInfo(): Promise<AppInfo>;
  getConfig(): Promise<ConfigEnvelope>;
  saveConfig(config: Config, baseRevision: number): Promise<ConfigEnvelope>;
  getRuntimeView(): Promise<RuntimeView>;
  setPin(key: string | null): Promise<void>;
  togglePin(key: string): Promise<void>;
  setPaused(paused: boolean): Promise<void>;
  reconnectDiscord(): Promise<void>;
  rescanNow(): Promise<void>;
  previewPresence(request: PreviewRequest): Promise<PreviewResult>;
  getIcons(keys: string[]): Promise<Record<string, string | null>>;
  getStartupStatus(): Promise<StartupStatus>;
  exportConfig(): Promise<string | null>;
  importConfigPreview(): Promise<ImportPreview | null>;
  applyImport(token: string, baseRevision: number): Promise<ConfigEnvelope>;
  resetConfig(baseRevision: number): Promise<ConfigEnvelope>;
  openLogFolder(): Promise<void>;
  exportDiagnostics(includeSensitive: boolean): Promise<string | null>;
  openExternal(link: ExternalLink): Promise<void>;
  quit(): Promise<void>;
  onRuntime(callback: (view: RuntimeView) => void): Promise<Unlisten>;
  onConfig(callback: (envelope: ConfigEnvelope) => void): Promise<Unlisten>;
}

export const EVENT_RUNTIME = "deskpresence://runtime";
export const EVENT_CONFIG = "deskpresence://config";

export function isCommandError(value: unknown): value is CommandError {
  return typeof value === "object" && value !== null && "code" in value && "message" in value;
}

/** Converts anything thrown by a command into a displayable error. */
export function toCommandError(value: unknown): CommandError {
  if (isCommandError(value)) return value;
  const message = value instanceof Error ? value.message : typeof value === "string" ? value : "Something went wrong.";
  return { code: "internal", message, issues: [] };
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function createTauriBackend(): Promise<Backend> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const call = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      throw toCommandError(error);
    }
  };
  return {
    kind: "tauri",
    getAppInfo: () => call("get_app_info"),
    getConfig: () => call("get_config"),
    saveConfig: (config, baseRevision) => call("save_config", { config, baseRevision }),
    getRuntimeView: () => call("get_runtime_view"),
    setPin: (key) => call("set_pin", { key }),
    togglePin: (key) => call("toggle_pin", { key }),
    setPaused: (paused) => call("set_paused", { paused }),
    reconnectDiscord: () => call("reconnect_discord"),
    rescanNow: () => call("rescan_now"),
    previewPresence: (request) => call("preview_presence", { request }),
    getIcons: (keys) => call("get_icons", { keys }),
    getStartupStatus: () => call("get_startup_status"),
    exportConfig: () => call("export_config"),
    importConfigPreview: () => call("import_config_preview"),
    applyImport: (token, baseRevision) => call("apply_import", { token, baseRevision }),
    resetConfig: (baseRevision) => call("reset_config", { baseRevision }),
    openLogFolder: () => call("open_log_folder"),
    exportDiagnostics: (includeSensitive) => call("export_diagnostics", { includeSensitive }),
    openExternal: (link) => call("open_external", { link }),
    quit: () => call("quit_app"),
    onRuntime: (callback) => listen<RuntimeView>(EVENT_RUNTIME, (event) => callback(event.payload)),
    onConfig: (callback) => listen<ConfigEnvelope>(EVENT_CONFIG, (event) => callback(event.payload)),
  };
}
