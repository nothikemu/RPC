import { useEffect, useState } from "react";
import { ArrowRight, Pause, PinOff, Play, RefreshCw } from "lucide-react";
import type { PageId } from "../App";
import { AppIcon } from "../components/AppIcon";
import { PresenceCard } from "../components/PresenceCard";
import { Badge, Button, Callout, Card, PageHeader, StatusDot } from "../components/ui";
import { formatRelative } from "../lib/format";
import { connectionSummary, deliveryLabel, detectorSummary, selectionReasonLabel } from "../lib/labels";
import type { StartupStatus } from "../lib/types";
import { useStore } from "../state/store";
import { useNow } from "../components/useNow";

export function OverviewPage({ onNavigate }: { onNavigate: (page: PageId) => void }) {
  const { backend, view, envelope, quickSave, notify } = useStore();
  const [startup, setStartup] = useState<StartupStatus | null>(null);
  const now = useNow(15_000);

  useEffect(() => {
    let active = true;
    backend
      .getStartupStatus()
      .then((s) => active && setStartup(s))
      .catch(() => active && setStartup(null));
    return () => {
      active = false;
    };
  }, [backend, envelope?.revision]);

  if (!view || !envelope) return null;
  const { presence, discord } = view;
  const connection = connectionSummary(discord);
  const detector = detectorSummary(view.detector.status);
  const selected = presence.selected;
  const selectedApp = selected ? view.apps.find((a) => a.key === selected.key) : undefined;
  const lastEvent = view.events[0];
  const connected = discord.phase === "connected";

  const run = async (action: () => Promise<void>, failure: string) => {
    try {
      await action();
    } catch {
      notify("error", failure);
    }
  };

  let emptyText = "Nothing to show. Focus an application to see it here.";
  if (presence.paused) emptyText = "Presence is paused. Discord shows nothing from DeskPresence.";
  else if (view.detector.running_count === 0) emptyText = "No applications detected yet.";

  let deliveryText: string;
  if (!presence.activity) deliveryText = presence.paused ? "Paused" : "Activity cleared";
  else if (connected) deliveryText = deliveryLabel[discord.delivery];
  else deliveryText = "Preview only — not sent to Discord";

  return (
    <>
      <PageHeader
        title="Overview"
        description="What DeskPresence is showing on Discord right now."
        actions={
          <Button
            variant={presence.paused ? "primary" : "secondary"}
            icon={presence.paused ? Play : Pause}
            onClick={() => void run(() => backend.setPaused(!presence.paused), "Couldn't change the pause state.")}
          >
            {presence.paused ? "Resume presence" : "Pause presence"}
          </Button>
        }
      />

      <div className="stack">
        {envelope.notice ? (
          <Callout tone={envelope.notice.kind === "recovered" ? "error" : "info"} title={envelope.notice.kind === "recovered" ? "Settings were reset" : "Settings upgraded"}>
            {envelope.notice.message}
            {envelope.notice.backup_path ? <span className="muted"> A copy of the previous file was saved to {envelope.notice.backup_path}.</span> : null}
          </Callout>
        ) : null}

        {!envelope.config.ui.tray_hint_dismissed ? (
          <Callout
            tone="info"
            title="DeskPresence keeps running in the system tray"
            actions={
              <Button size="sm" onClick={() => void quickSave((c) => ({ ...c, ui: { ...c.ui, tray_hint_dismissed: true } }))}>
                Got it
              </Button>
            }
          >
            Closing this window hides it. Right-click the tray icon to pin an application, pause presence, or exit completely.
          </Callout>
        ) : null}

        {view.detector.status.state === "unsupported" || view.detector.status.state === "mock" || view.detector.status.state === "degraded" ? (
          <Callout tone="warning" title={`Application detection: ${detector.label}`}>
            {detector.detail}
          </Callout>
        ) : null}

        <div className="overview-grid">
          <Card
            className="overview-presence"
            title="Current presence"
            actions={
              presence.pinned ? (
                <Button size="sm" variant="ghost" icon={PinOff} onClick={() => void run(() => backend.setPin(null), "Couldn't unpin.")}>
                  Unpin
                </Button>
              ) : null
            }
          >
            <div className="source-line">
              {selected ? (
                <>
                  <AppIcon backend={backend} appKey={selected.key} size={28} />
                  <div className="source-text">
                    <span className="source-name">{selected.display_name}</span>
                    <span className="source-exe">{selectedApp?.exe_name}</span>
                  </div>
                  <Badge tone={selected.reason === "pinned" ? "accent" : "neutral"}>{selectionReasonLabel[selected.reason]}</Badge>
                </>
              ) : (
                <span className="muted">No application selected</span>
              )}
            </div>
            <PresenceCard activity={presence.activity} emptyText={emptyText} />
            <div className="delivery-line">
              <StatusDot tone={presence.activity && connected ? (discord.delivery === "rejected" ? "danger" : "success") : "neutral"} />
              <span>{deliveryText}</span>
            </div>
            {connected && presence.activity ? (
              <p className="field-description">Discord confirms that its app on this computer accepted the activity, not what other people see.</p>
            ) : null}
            {presence.warnings.length > 0 ? (
              <ul className="warning-list">
                {presence.warnings.map((w, i) => (
                  <li key={`${w.field}-${i}`}>
                    <strong>{w.field.replace("_", " ")}:</strong> {w.message}
                  </li>
                ))}
              </ul>
            ) : null}
            {discord.last_delivery_error ? <p className="field-hint warning">{discord.last_delivery_error}</p> : null}
          </Card>

          <div className="tile-column">
            <Card className="tile">
              <p className="tile-label">Discord</p>
              <p className="tile-value">
                <StatusDot tone={connection.tone} />
                {connection.label}
              </p>
              <p className="tile-detail">{connection.detail}</p>
              {discord.phase === "not_configured" || discord.phase === "invalid_application_id" ? (
                <Button size="sm" variant="secondary" onClick={() => onNavigate("settings")}>
                  Open Discord settings
                </Button>
              ) : !connected ? (
                <Button size="sm" variant="secondary" icon={RefreshCw} onClick={() => void run(() => backend.reconnectDiscord(), "Couldn't reconnect.")}>
                  Retry now
                </Button>
              ) : null}
            </Card>
            <div className="tile-row">
              <Card className="tile">
                <p className="tile-label">Detected apps</p>
                <p className="tile-value tile-number">{view.detector.running_count}</p>
                <button type="button" className="link-btn" onClick={() => onNavigate("applications")}>
                  View applications <ArrowRight size={14} aria-hidden />
                </button>
              </Card>
              <Card className="tile">
                <p className="tile-label">Startup</p>
                <p className="tile-value">
                  {startup == null ? "—" : startup.registered ? "Launches at sign-in" : "Off"}
                </p>
                <button type="button" className="link-btn" onClick={() => onNavigate("settings")}>
                  Change <ArrowRight size={14} aria-hidden />
                </button>
              </Card>
            </div>
            <Card className="tile">
              <p className="tile-label">Latest event</p>
              {lastEvent ? (
                <>
                  <p className="tile-event">{lastEvent.message}</p>
                  <p className="tile-detail">
                    {formatRelative(lastEvent.at_ms, now)}
                    {lastEvent.count > 1 ? ` · ${lastEvent.count} times` : ""}
                  </p>
                </>
              ) : (
                <p className="tile-detail">No events yet.</p>
              )}
              <button type="button" className="link-btn" onClick={() => onNavigate("diagnostics")}>
                Open diagnostics <ArrowRight size={14} aria-hidden />
              </button>
            </Card>
          </div>
        </div>
      </div>
    </>
  );
}
