import { useState } from "react";
import { CircleAlert, Download, FolderOpen, Info, RefreshCw, TriangleAlert } from "lucide-react";
import { Badge, Button, Card, KeyValue, PageHeader, StatusDot } from "../components/ui";
import { useNow } from "../components/useNow";
import { toCommandError } from "../lib/backend";
import { formatDateTime, formatMicros, formatRelative, formatTime } from "../lib/format";
import { connectionSummary, deliveryLabel, detectorSummary, selectionReasonLabel } from "../lib/labels";
import { useStore } from "../state/store";

const levelIcon = { info: Info, warning: TriangleAlert, error: CircleAlert } as const;

export function DiagnosticsPage() {
  const { backend, view, info, notify } = useStore();
  const [includeSensitive, setIncludeSensitive] = useState(false);
  const [exporting, setExporting] = useState(false);
  const now = useNow(5_000);
  if (!view) return null;
  const { discord, detector, presence } = view;
  const connection = connectionSummary(discord);
  const detection = detectorSummary(detector.status);

  const exportBundle = async () => {
    setExporting(true);
    try {
      const path = await backend.exportDiagnostics(includeSensitive);
      if (path) notify("success", `Diagnostics saved to ${path}`);
    } catch (error) {
      notify("error", toCommandError(error).message);
    } finally {
      setExporting(false);
    }
  };

  return (
    <>
      <PageHeader
        title="Diagnostics"
        description="Live status and recent events. Everything here stays on this computer unless you export it."
        actions={
          <Button icon={FolderOpen} onClick={() => void backend.openLogFolder().catch((e) => notify("error", toCommandError(e).message))}>
            Open log folder
          </Button>
        }
      />
      <div className="stack">
        <div className="grid-2 align-start">
          <Card
            title={
              <>
                <StatusDot tone={connection.tone} /> Discord
              </>
            }
            actions={
              <Button size="sm" icon={RefreshCw} onClick={() => void backend.reconnectDiscord()}>
                Reconnect
              </Button>
            }
          >
            <KeyValue
              items={[
                { label: "Status", value: connection.label },
                { label: "Endpoint", value: discord.endpoint ?? "—" },
                { label: "Last attempt", value: discord.last_attempt_at_ms ? `${formatTime(discord.last_attempt_at_ms)} (${formatRelative(discord.last_attempt_at_ms, now)})` : "—" },
                { label: "Next retry", value: discord.next_retry_at_ms ? formatRelative(discord.next_retry_at_ms, now) : "—" },
                { label: "Connected since", value: formatDateTime(discord.connected_since_ms) },
                { label: "Reconnects", value: String(discord.reconnects) },
                { label: "Failed attempts", value: String(discord.failed_attempts) },
                { label: "Last error", value: discord.last_error ?? "None" },
                { label: "Last activity update", value: discord.last_activity_update_at_ms ? `${formatTime(discord.last_activity_update_at_ms)} — ${deliveryLabel[discord.delivery]}` : deliveryLabel[discord.delivery] },
                { label: "Application ID", value: view.application_id.effective ? `${view.application_id.effective} (${view.application_id.source === "config" ? "from settings" : "built in"})` : "Not set" },
              ]}
            />
          </Card>
          <Card
            title={
              <>
                <StatusDot tone={detection.tone} /> Detection
              </>
            }
            actions={
              <Button size="sm" icon={RefreshCw} onClick={() => void backend.rescanNow()}>
                Scan now
              </Button>
            }
          >
            <KeyValue
              items={[
                { label: "Status", value: detection.label },
                { label: "Details", value: detection.detail },
                { label: "Running applications", value: String(detector.running_count) },
                { label: "Scans", value: String(detector.stats.scans) },
                { label: "Last scan", value: detector.stats.scans ? `${formatMicros(detector.stats.last_scan_duration_us)} (max ${formatMicros(detector.stats.max_scan_duration_us)})` : "—" },
                { label: "Presence source", value: presence.selected ? `${presence.selected.display_name} — ${selectionReasonLabel[presence.selected.reason]}` : "None" },
                { label: "Selection mode", value: presence.mode === "foreground" ? "Foreground" : "Priority" },
                { label: "Paused", value: presence.paused ? "Yes" : "No" },
                { label: "Running since", value: formatDateTime(view.started_at_ms) },
                { label: "Version", value: info ? `${info.version} (${info.platform})` : "—" },
              ]}
            />
          </Card>
        </div>

        <Card title="Recent events" description="Repeated events are grouped.">
          {view.events.length === 0 ? (
            <p className="muted">No events yet.</p>
          ) : (
            <ul className="event-list">
              {view.events.map((event) => {
                const Icon = levelIcon[event.level];
                return (
                  <li key={event.id} className={`event event-${event.level}`}>
                    <Icon size={15} aria-hidden className="event-icon" />
                    <span className="event-time" title={new Date(event.at_ms).toLocaleString()}>
                      {formatTime(event.at_ms)}
                    </span>
                    <Badge>{event.category}</Badge>
                    <span className="event-message">{event.message}</span>
                    {event.count > 1 ? <span className="event-count">×{event.count}</span> : null}
                  </li>
                );
              })}
            </ul>
          )}
        </Card>

        <Card title="Export diagnostics" description="Creates a JSON file you can attach to a bug report.">
          <p className="field-description">
            By default, user folder names in paths, presence text (which may contain window titles) and your Discord user name are removed.
          </p>
          <label className="checkbox">
            <input type="checkbox" checked={includeSensitive} onChange={(e) => setIncludeSensitive(e.target.checked)} />
            Include full paths, presence text and Discord user name
          </label>
          <div className="button-row">
            <Button icon={Download} onClick={() => void exportBundle()} loading={exporting}>
              Export diagnostics…
            </Button>
          </div>
        </Card>
      </div>
    </>
  );
}
