// Human-readable labels for backend enums, kept in one place for consistency.
import type {
  ActivityType,
  AppRuleStatus,
  ConnectionPhase,
  DetectorStatus,
  DiscordStatus,
  ExcludedFocusBehavior,
  LogLevel,
  MatchKind,
  SelectionMode,
  SelectionReason,
  StatusDisplay,
  Theme,
  TimestampMode,
} from "./types";

export type Tone = "neutral" | "accent" | "success" | "warning" | "danger";

export const selectionReasonLabel: Record<SelectionReason, string> = {
  pinned: "Pinned manually",
  foreground: "Focused window",
  recent_foreground: "Most recently focused",
  priority: "Highest priority",
  priority_fallback: "Focused window (priority fallback)",
};

export const selectionModeLabel: Record<SelectionMode, string> = {
  foreground: "Foreground",
  priority: "Priority",
};

export const matchKindLabel: Record<MatchKind, string> = {
  path: "Exact path",
  exe_name: "Executable name",
  pattern: "Pattern",
};

export const ruleStatusLabel: Record<AppRuleStatus, { label: string; tone: Tone }> = {
  automatic: { label: "Automatic", tone: "neutral" },
  configured: { label: "Custom rule", tone: "accent" },
  disabled: { label: "Disabled", tone: "warning" },
  excluded: { label: "Excluded", tone: "danger" },
};

export const activityTypeLabel: Record<ActivityType, string> = {
  playing: "Playing",
  listening: "Listening to",
  watching: "Watching",
  competing: "Competing in",
};

export const statusDisplayLabel: Record<StatusDisplay, string> = {
  name: "Discord application name",
  state: "State text",
  details: "Details text",
};

export const timestampModeLabel: Record<TimestampMode, { label: string; description: string }> = {
  since_selected: {
    label: "Reset when the app changes",
    description: "The timer restarts whenever a different application takes over the presence.",
  },
  since_session: {
    label: "Keep counting across apps",
    description: "The timer keeps running while any application is shown, until presence clears.",
  },
  since_launch: {
    label: "Since the app was launched",
    description: "Uses the selected application's process start time.",
  },
  off: { label: "Don't show elapsed time", description: "No timestamp is sent to Discord." },
};

export const excludedFocusLabel: Record<ExcludedFocusBehavior, string> = {
  keep_previous: "Keep showing the previous app",
  clear: "Clear the presence",
};

export const themeLabel: Record<Theme, string> = { system: "System", light: "Light", dark: "Dark" };

export const logLevelLabel: Record<LogLevel, string> = {
  error: "Errors only",
  warn: "Warnings",
  info: "Normal",
  debug: "Detailed",
  trace: "Everything (very verbose)",
};

export function connectionSummary(status: DiscordStatus): { label: string; tone: Tone; detail: string } {
  const phase: ConnectionPhase = status.phase;
  switch (phase) {
    case "connected":
      return {
        label: "Connected",
        tone: "success",
        detail: status.user_name ? `Connected to the Discord client as ${status.user_name}.` : "Connected to the Discord client.",
      };
    case "connecting":
      return { label: "Connecting…", tone: "accent", detail: "Looking for the Discord desktop client." };
    case "not_configured":
      return {
        label: "Not set up",
        tone: "neutral",
        detail: "Add a Discord Application ID to send your presence to Discord. Until then, it's shown here as a preview only.",
      };
    case "invalid_application_id":
      return {
        label: "ID rejected",
        tone: "danger",
        detail: "Discord rejected the Application ID. Check that it matches an application in the Developer Portal.",
      };
    case "waiting_to_retry":
      if (status.last_error_kind === "not_running") {
        return {
          label: "Discord not running",
          tone: "warning",
          detail:
            status.discord_installed === false
              ? "The Discord desktop app doesn't appear to be installed. DeskPresence keeps retrying in the background."
              : "Start the Discord desktop app. DeskPresence connects automatically.",
        };
      }
      return {
        label: "Reconnecting",
        tone: "warning",
        detail: status.last_error ?? "The connection to Discord failed. Retrying automatically.",
      };
    case "stopped":
      return { label: "Stopped", tone: "neutral", detail: "The Discord connection is stopped." };
  }
}

export function detectorSummary(status: DetectorStatus): { label: string; tone: Tone; detail: string } {
  switch (status.state) {
    case "running":
      return { label: "Running", tone: "success", detail: `Detecting visible applications (${status.backend}).` };
    case "starting":
      return { label: "Starting", tone: "accent", detail: "Detection is starting." };
    case "mock":
      return { label: "Mock data", tone: "warning", detail: "Showing sample applications for development. These are not real." };
    case "unsupported":
      return { label: "Unsupported", tone: "warning", detail: status.reason };
    case "degraded":
      return { label: "Degraded", tone: "danger", detail: status.message };
    case "stopped":
      return { label: "Stopped", tone: "neutral", detail: "Detection is stopped." };
  }
}

export const deliveryLabel: Record<DiscordStatus["delivery"], string> = {
  idle: "Nothing sent yet",
  pending: "Waiting for Discord",
  accepted: "Accepted by the Discord client",
  accepted_without_assets: "Accepted without artwork",
  rejected: "Rejected by Discord",
  cleared: "Cleared",
};
