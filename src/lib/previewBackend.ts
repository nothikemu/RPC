// In-memory backend used only when the UI runs outside DeskPresence (a plain
// browser via `npm run dev`) and in frontend tests. The UI shows a visible
// "Browser preview" banner whenever this backend is active. Nothing here talks
// to Windows or Discord; the applications below are fictional samples.
import type { Backend, Unlisten } from "./backend";
import { renderTemplate, templateError, usesVariable } from "./templates";
import type {
  Activity,
  AppView,
  CommandError,
  Config,
  ConfigEnvelope,
  ImportPreview,
  PreviewRequest,
  PreviewResult,
  RuntimeView,
  StartupStatus,
  ValidationIssue,
} from "./types";
import { applicationIdError, assetValueError, matcherError } from "./validation";

export function defaultConfig(): Config {
  return {
    schema_version: 1,
    discord: { application_id: null, activity_type: "playing", status_display: "name", override_activity_name: false },
    presence: {
      details_template: "Using {app}",
      state_template: "",
      timestamp_mode: "since_selected",
      selection_mode: "foreground",
      excluded_focus: "keep_previous",
      priority_fallback_to_foreground: false,
      default_assets: { large_image: null, large_text: null, small_image: null, small_text: null },
    },
    rules: [],
    exclusions: [],
    detection: { poll_interval_ms: 1000, foreground_debounce_ms: 500 },
    privacy: { capture_window_titles: false },
    startup: { launch_at_login: false, start_minimized: false },
    window: { close_to_tray: true },
    ui: { theme: "system", onboarding_completed: false, tray_hint_dismissed: false },
    diagnostics: { log_level: "info" },
  };
}

const SAMPLE_APPS: Array<{ name: string; exe: string; path: string }> = [
  { name: "Sample Editor", exe: "editor.exe", path: "C:\\Samples\\Editor\\editor.exe" },
  { name: "Sample Browser", exe: "browser.exe", path: "C:\\Samples\\Browser\\browser.exe" },
  { name: "Sample Music", exe: "music.exe", path: "C:\\Samples\\Music\\music.exe" },
];

function keyOf(path: string): string {
  return path.toLowerCase();
}

function validate(config: Config): ValidationIssue[] {
  const issues: ValidationIssue[] = [];
  const id = config.discord.application_id;
  if (id != null) {
    const e = applicationIdError(id);
    if (e) issues.push({ path: "discord.application_id", message: e });
  }
  for (const [path, source] of [
    ["presence.details_template", config.presence.details_template],
    ["presence.state_template", config.presence.state_template],
  ] as const) {
    const e = templateError(source);
    if (e) issues.push({ path, message: e.message });
  }
  config.rules.forEach((rule, i) => {
    const e = matcherError(rule.matcher.kind, rule.matcher.value);
    if (e) issues.push({ path: `rules[${i}].matcher`, message: e });
    for (const field of ["large_image", "small_image"] as const) {
      const v = rule.assets[field];
      const ae = v ? assetValueError(v) : null;
      if (ae) issues.push({ path: `rules[${i}].assets.${field}`, message: ae });
    }
  });
  config.exclusions.forEach((x, i) => {
    const e = matcherError(x.matcher.kind, x.matcher.value);
    if (e) issues.push({ path: `exclusions[${i}].matcher`, message: e });
  });
  return issues;
}

export class PreviewBackend implements Backend {
  readonly kind = "preview" as const;
  private envelope: ConfigEnvelope;
  private paused = false;
  private pinned: string | null = null;
  private foreground = 0;
  private runtimeListeners = new Set<(view: RuntimeView) => void>();
  private configListeners = new Set<(envelope: ConfigEnvelope) => void>();
  private readonly startedAt = Date.now();
  private sourceSince = Date.now();
  calls: Array<{ method: string; args: unknown[] }> = [];

  constructor(initial?: Partial<Config>) {
    this.envelope = { config: { ...defaultConfig(), ...initial }, revision: 1, notice: null };
  }

  private record(method: string, ...args: unknown[]) {
    this.calls.push({ method, args });
  }

  private emitRuntime() {
    const view = this.buildView();
    this.runtimeListeners.forEach((l) => l(view));
  }

  private buildView(): RuntimeView {
    const config = this.envelope.config;
    const apps: AppView[] = SAMPLE_APPS.map((s, i) => {
      const key = keyOf(s.path);
      const rule = config.rules.find((r) => r.matcher.value.toLowerCase() === key || r.matcher.value.toLowerCase() === s.exe);
      const excluded = config.exclusions.find(
        (x) => x.matcher.value.toLowerCase() === key || x.matcher.value.toLowerCase() === s.exe,
      );
      return {
        key,
        display_name: rule?.display_name ?? s.name,
        name_source: rule?.display_name ? "user" : "metadata",
        exe_name: s.exe,
        exe_path: s.path,
        running: true,
        is_foreground: i === this.foreground,
        is_selected: false,
        is_pinned: this.pinned === key,
        window_count: 1,
        first_seen_ms: this.startedAt,
        last_seen_ms: null,
        last_focused_ms: i === this.foreground ? Date.now() : null,
        status: excluded ? "excluded" : rule ? (rule.enabled ? "configured" : "disabled") : "automatic",
        rule_id: rule?.id ?? null,
        rule_match: rule?.matcher.kind ?? null,
        exclusion_id: excluded?.id ?? null,
        priority: rule?.priority ?? 0,
        path_exists: null,
      };
    });
    const eligible = apps.filter((a) => a.status === "automatic" || a.status === "configured");
    const selected =
      eligible.find((a) => a.key === this.pinned) ?? eligible.find((a) => a.is_foreground) ?? eligible[0] ?? null;
    if (selected) selected.is_selected = true;
    const activity: Activity | null =
      selected && !this.paused ? this.renderActivity(config, selected.display_name, selected.exe_name) : null;
    return {
      revision: Date.now(),
      started_at_ms: this.startedAt,
      detector: {
        status: { state: "mock" },
        stats: { scans: 1, last_scan_at_ms: Date.now(), last_scan_duration_us: 800, max_scan_duration_us: 1200 },
        running_count: apps.length,
      },
      apps,
      presence: {
        paused: this.paused,
        mode: config.presence.selection_mode,
        pinned: this.pinned,
        selected: selected
          ? { key: selected.key, display_name: selected.display_name, reason: this.pinned === selected.key ? "pinned" : "foreground" }
          : null,
        activity,
        warnings: [],
        source_since_ms: selected ? this.sourceSince : null,
      },
      discord: {
        phase: "not_configured",
        endpoint: null,
        user_name: null,
        failed_attempts: 0,
        reconnects: 0,
        last_attempt_at_ms: null,
        next_retry_at_ms: null,
        connected_since_ms: null,
        last_error: null,
        last_error_kind: null,
        discord_installed: null,
        delivery: "idle",
        last_delivery_error: null,
        last_activity_update_at_ms: null,
      },
      application_id: {
        effective: config.discord.application_id,
        source: config.discord.application_id ? "config" : "none",
      },
      events: [
        {
          id: 1,
          at_ms: this.startedAt,
          level: "warning",
          category: "app",
          message: "Browser preview: sample data only. Run DeskPresence through Tauri for real detection.",
          count: 1,
        },
      ],
    };
  }

  private renderActivity(config: Config, app: string, exe: string): Activity {
    const values = { app, exe, elapsed: "<1m", window_title: "" };
    const details = renderTemplate(config.presence.details_template, values) ?? undefined;
    const state = renderTemplate(config.presence.state_template, values) ?? undefined;
    return {
      type: 0,
      details: details && details.length >= 2 ? details.slice(0, 128) : undefined,
      state: state && state.length >= 2 ? state.slice(0, 128) : undefined,
      timestamps: config.presence.timestamp_mode === "off" ? undefined : { start: Math.floor(this.sourceSince / 1000) },
    };
  }

  async getAppInfo() {
    return {
      product_name: "DeskPresence",
      version: "preview",
      platform: "browser",
      detection_supported: false,
      has_build_default_app_id: false,
      config_path: "(in memory)",
      log_dir: "(none)",
    };
  }

  async getConfig() {
    return structuredClone(this.envelope);
  }

  async saveConfig(config: Config, baseRevision: number): Promise<ConfigEnvelope> {
    this.record("saveConfig", config, baseRevision);
    if (baseRevision !== this.envelope.revision) {
      const error: CommandError = { code: "conflict", message: "The configuration changed in the meantime.", issues: [] };
      throw error;
    }
    const issues = validate(config);
    if (issues.length > 0) {
      const error: CommandError = {
        code: "invalid",
        message: `${issues[0]?.path}: ${issues[0]?.message}`,
        issues,
      };
      throw error;
    }
    this.envelope = { config: structuredClone(config), revision: this.envelope.revision + 1, notice: null };
    const snapshot = structuredClone(this.envelope);
    this.configListeners.forEach((l) => l(snapshot));
    this.emitRuntime();
    return snapshot;
  }

  async getRuntimeView() {
    return this.buildView();
  }

  async setPin(key: string | null) {
    this.record("setPin", key);
    this.pinned = key;
    this.emitRuntime();
  }

  async togglePin(key: string) {
    this.record("togglePin", key);
    this.pinned = this.pinned === key ? null : key;
    this.sourceSince = Date.now();
    this.emitRuntime();
  }

  async setPaused(paused: boolean) {
    this.record("setPaused", paused);
    this.paused = paused;
    this.emitRuntime();
  }

  async reconnectDiscord() {
    this.record("reconnectDiscord");
  }

  async rescanNow() {
    this.record("rescanNow");
    this.foreground = (this.foreground + 1) % SAMPLE_APPS.length;
    this.sourceSince = Date.now();
    this.emitRuntime();
  }

  async previewPresence(request: PreviewRequest): Promise<PreviewResult> {
    const view = this.buildView();
    const app = view.apps.find((a) => a.key === request.app_key) ?? view.apps.find((a) => a.is_selected);
    const values = {
      app: app?.display_name ?? "Example Editor",
      exe: app?.exe_name ?? "example.exe",
      elapsed: "25m",
      window_title: this.envelope.config.privacy.capture_window_titles ? "notes.txt - Sample Editor" : "",
    };
    const toError = (source: string) => {
      const e = templateError(source);
      return e ? { kind: "unclosed" as const, position: e.position } : null;
    };
    const warnings = [request.details_template, request.state_template].flatMap((source, i) =>
      usesVariable(source, "window_title") && !this.envelope.config.privacy.capture_window_titles
        ? [{ field: i === 0 ? ("details" as const) : ("state" as const), message: "{window_title} is empty because window-title tracking is off." }]
        : [],
    );
    return {
      app_display_name: values.app,
      app_exe_name: values.exe,
      sample: !app,
      details: renderTemplate(request.details_template, values) || null,
      state: renderTemplate(request.state_template, values) || null,
      details_error: toError(request.details_template),
      state_error: toError(request.state_template),
      warnings,
    };
  }

  async getIcons(keys: string[]) {
    return Object.fromEntries(keys.map((k) => [k, null]));
  }

  async getStartupStatus(): Promise<StartupStatus> {
    return { registered: this.envelope.config.startup.launch_at_login, preference: this.envelope.config.startup.launch_at_login, error: null };
  }

  async exportConfig() {
    this.record("exportConfig");
    return null;
  }

  async importConfigPreview(): Promise<ImportPreview | null> {
    this.record("importConfigPreview");
    return null;
  }

  async applyImport(): Promise<ConfigEnvelope> {
    throw { code: "not_found", message: "Import is not available in the browser preview.", issues: [] } satisfies CommandError;
  }

  async resetConfig(baseRevision: number) {
    this.record("resetConfig", baseRevision);
    const fresh = defaultConfig();
    fresh.ui.onboarding_completed = this.envelope.config.ui.onboarding_completed;
    return this.saveConfig(fresh, baseRevision);
  }

  async openLogFolder() {
    this.record("openLogFolder");
  }

  async exportDiagnostics(includeSensitive: boolean) {
    this.record("exportDiagnostics", includeSensitive);
    return null;
  }

  async openExternal(link: string) {
    this.record("openExternal", link);
  }

  async quit() {
    this.record("quit");
  }

  async onRuntime(callback: (view: RuntimeView) => void): Promise<Unlisten> {
    this.runtimeListeners.add(callback);
    return () => this.runtimeListeners.delete(callback);
  }

  async onConfig(callback: (envelope: ConfigEnvelope) => void): Promise<Unlisten> {
    this.configListeners.add(callback);
    return () => this.configListeners.delete(callback);
  }
}
