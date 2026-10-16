// Re-exports of the TypeScript bindings generated from the Rust types
// (`npm run bindings`). Do not hand-edit files in ./bindings.
export type { Activity } from "./bindings/Activity";
export type { ActivityType } from "./bindings/ActivityType";
export type { AppInfo } from "./bindings/AppInfo";
export type { AppKey } from "./bindings/AppKey";
export type { AppMatcher } from "./bindings/AppMatcher";
export type { AppRule } from "./bindings/AppRule";
export type { AppRuleStatus } from "./bindings/AppRuleStatus";
export type { AppView } from "./bindings/AppView";
export type { AssetOverrides } from "./bindings/AssetOverrides";
export type { CommandError } from "./bindings/CommandError";
export type { Config } from "./bindings/Config";
export type { ConfigEnvelope } from "./bindings/ConfigEnvelope";
export type { ConnectionPhase } from "./bindings/ConnectionPhase";
export type { DetectorStatus } from "./bindings/DetectorStatus";
export type { DiagnosticEvent } from "./bindings/DiagnosticEvent";
export type { DiscordStatus } from "./bindings/DiscordStatus";
export type { ExcludedFocusBehavior } from "./bindings/ExcludedFocusBehavior";
export type { ExclusionRule } from "./bindings/ExclusionRule";
export type { ImportPreview } from "./bindings/ImportPreview";
export type { LogLevel } from "./bindings/LogLevel";
export type { MatchKind } from "./bindings/MatchKind";
export type { PresenceWarning } from "./bindings/PresenceWarning";
export type { PreviewRequest } from "./bindings/PreviewRequest";
export type { PreviewResult } from "./bindings/PreviewResult";
export type { RuntimeView } from "./bindings/RuntimeView";
export type { SelectionMode } from "./bindings/SelectionMode";
export type { SelectionReason } from "./bindings/SelectionReason";
export type { StartupStatus } from "./bindings/StartupStatus";
export type { StatusDisplay } from "./bindings/StatusDisplay";
export type { TemplateError } from "./bindings/TemplateError";
export type { Theme } from "./bindings/Theme";
export type { TimestampMode } from "./bindings/TimestampMode";
export type { ValidationIssue } from "./bindings/ValidationIssue";

/** Links the backend is allowed to open (URLs are fixed in Rust). */
export type ExternalLink = "developer_portal" | "rich_presence_docs" | "rpc_docs";
