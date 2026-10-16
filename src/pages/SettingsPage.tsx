import { useEffect, useId, useState } from "react";
import { Download, ExternalLink, LogOut, Monitor, Moon, RefreshCw, RotateCcw, Sparkles, Sun, Upload } from "lucide-react";
import { Dialog } from "../components/Overlay";
import { Badge, Button, Callout, Card, Field, PageHeader, SegmentedControl, Select, StatusDot, Switch, TextInput } from "../components/ui";
import { toCommandError } from "../lib/backend";
import { connectionSummary, logLevelLabel } from "../lib/labels";
import type { ImportPreview, LogLevel, StartupStatus, Theme } from "../lib/types";
import { applicationIdError, clamp, DEBOUNCE, POLL_INTERVAL } from "../lib/validation";
import { useIssue, useStore } from "../state/store";

function NumberField({
  id,
  label,
  description,
  value,
  min,
  max,
  onChange,
}: {
  id: string;
  label: string;
  description: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}) {
  const [text, setText] = useState(String(value));
  const [synced, setSynced] = useState(value);
  // Follow external changes (discard, import, reset) without an effect.
  if (value !== synced) {
    setSynced(value);
    setText(String(value));
  }
  const parsed = Number.parseInt(text, 10);
  const error = text.trim() === "" || Number.isNaN(parsed) ? "Enter a number." : parsed < min || parsed > max ? `Use a value from ${min} to ${max}.` : null;
  return (
    <Field label={label} htmlFor={id} description={description} error={error}>
      <div className="input-with-unit">
        <TextInput
          id={id}
          type="number"
          inputMode="numeric"
          min={min}
          max={max}
          value={text}
          invalid={!!error}
          className="input-narrow"
          onChange={(e) => {
            setText(e.target.value);
            const n = Number.parseInt(e.target.value, 10);
            if (!Number.isNaN(n) && n >= min && n <= max) onChange(n);
          }}
          onBlur={() => {
            const n = clamp(Number.parseInt(text, 10), min, max);
            setText(String(n));
            onChange(n);
          }}
        />
        <span className="unit">ms</span>
      </div>
    </Field>
  );
}

export function SettingsPage({ onShowOnboarding }: { onShowOnboarding: () => void }) {
  const { backend, view, draft, envelope, dirty, updateDraft, replaceEnvelope, discardDraft, notify } = useStore();
  const [startup, setStartup] = useState<StartupStatus | null>(null);
  const [importPreview, setImportPreview] = useState<ImportPreview | null>(null);
  const [confirmReset, setConfirmReset] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const ids = { appId: useId(), poll: useId(), debounce: useId(), log: useId() };
  const appIdIssue = useIssue("discord.application_id");

  useEffect(() => {
    let active = true;
    backend.getStartupStatus().then((s) => active && setStartup(s)).catch(() => undefined);
    return () => {
      active = false;
    };
  }, [backend, envelope?.revision]);

  if (!draft || !view || !envelope) return null;
  const appId = draft.discord.application_id ?? "";
  const appIdError = applicationIdError(appId) ?? appIdIssue;
  const connection = connectionSummary(view.discord);
  const usingBuildDefault = view.application_id.source === "build_default" && !draft.discord.application_id;

  const run = async (name: string, action: () => Promise<void>) => {
    setBusy(name);
    try {
      await action();
    } catch (error) {
      notify("error", toCommandError(error).message);
    } finally {
      setBusy(null);
    }
  };

  const startImport = () =>
    run("import", async () => {
      const preview = await backend.importConfigPreview();
      if (preview) setImportPreview(preview);
    });

  const applyImport = () =>
    run("apply-import", async () => {
      if (!importPreview) return;
      const next = await backend.applyImport(importPreview.token, envelope.revision);
      discardDraft();
      replaceEnvelope(next);
      setImportPreview(null);
      notify("success", "Settings imported. A backup of your previous settings was saved.");
    });

  const reset = () =>
    run("reset", async () => {
      const next = await backend.resetConfig(envelope.revision);
      discardDraft();
      replaceEnvelope(next);
      setConfirmReset(false);
      notify("success", "Settings reset to defaults. A backup was saved.");
    });

  return (
    <>
      <PageHeader title="Settings" />
      <div className="stack">
        <Card title="Discord">
          <Callout tone="info" title="Two different identities">
            Discord shows the <strong>name and artwork of a Discord application</strong> you create in the Developer Portal (for example
            “Playing DeskPresence”). The <strong>detected Windows application</strong> (Zed, a browser, a game…) appears in the Details and
            State text below it. DeskPresence never needs a client secret or token — only the public Application ID.
          </Callout>
          <Field
            label="Discord Application ID"
            htmlFor={ids.appId}
            description={
              usingBuildDefault
                ? "This build includes a default Application ID. Enter your own to override it."
                : "Copy it from the Developer Portal → your application → General Information."
            }
            error={appIdError}
            aside={
              <button type="button" className="link-btn" onClick={() => void backend.openExternal("developer_portal")}>
                Developer Portal <ExternalLink size={13} aria-hidden />
              </button>
            }
          >
            <TextInput
              id={ids.appId}
              className="mono input-medium"
              inputMode="numeric"
              placeholder={usingBuildDefault ? view.application_id.effective ?? "" : "123456789012345678"}
              value={appId}
              invalid={!!appIdError}
              onChange={(e) => {
                const value = e.target.value.trim();
                updateDraft((c) => ({ ...c, discord: { ...c.discord, application_id: value === "" ? null : value } }));
              }}
            />
          </Field>
          <div className="status-row">
            <StatusDot tone={connection.tone} />
            <div>
              <p className="status-row-title">{connection.label}</p>
              <p className="field-description">{connection.detail} A valid-looking ID can still be rejected by Discord.</p>
            </div>
            <Button size="sm" icon={RefreshCw} onClick={() => void run("reconnect", () => backend.reconnectDiscord())} loading={busy === "reconnect"}>
              Reconnect
            </Button>
          </div>
        </Card>

        <Card title="Startup and window">
          <Switch
            checked={draft.startup.launch_at_login}
            onChange={(value) => updateDraft((c) => ({ ...c, startup: { ...c.startup, launch_at_login: value } }))}
            label={
              <>
                Launch at Windows startup{" "}
                {startup ? <Badge tone={startup.registered ? "success" : "neutral"}>{startup.registered ? "Registered" : "Not registered"}</Badge> : null}
              </>
            }
            description={
              startup?.error
                ? `Windows reported: ${startup.error}`
                : "Starts quietly in the tray when you sign in. You can also turn this off in Task Manager → Startup apps."
            }
          />
          <Switch
            checked={draft.startup.start_minimized}
            onChange={(value) => updateDraft((c) => ({ ...c, startup: { ...c.startup, start_minimized: value } }))}
            label="Start minimized to the tray"
            description="When you open DeskPresence yourself, start without showing this window."
          />
          <Switch
            checked={draft.window.close_to_tray}
            onChange={(value) => updateDraft((c) => ({ ...c, window: { close_to_tray: value } }))}
            label="Keep running when this window is closed"
            description="Closing the window hides it to the tray. Turn off to exit DeskPresence when the window closes."
          />
        </Card>

        <Card title="Detection">
          <div className="grid-2">
            <NumberField
              id={ids.poll}
              label="Scan interval"
              description="How often all windows are scanned. Focus changes are checked several times per second regardless."
              value={draft.detection.poll_interval_ms}
              min={POLL_INTERVAL.min}
              max={POLL_INTERVAL.max}
              onChange={(value) => updateDraft((c) => ({ ...c, detection: { ...c.detection, poll_interval_ms: value } }))}
            />
            <NumberField
              id={ids.debounce}
              label="Focus change delay"
              description="How long a newly focused app must stay focused before it takes over. Prevents flicker from brief popups."
              value={draft.detection.foreground_debounce_ms}
              min={DEBOUNCE.min}
              max={DEBOUNCE.max}
              onChange={(value) => updateDraft((c) => ({ ...c, detection: { ...c.detection, foreground_debounce_ms: value } }))}
            />
          </div>
        </Card>

        <Card title="Privacy">
          <Switch
            checked={draft.privacy.capture_window_titles}
            onChange={(value) => updateDraft((c) => ({ ...c, privacy: { capture_window_titles: value } }))}
            label="Allow window titles in presence text"
            description="Window titles can contain document names, web page titles or messages. When on, {window_title} is filled in and can be sent to Discord. Titles are never saved to disk or logged."
          />
          <div className="privacy-summary">
            <p className="field-label">What leaves your computer</p>
            <p className="field-description">
              Only the activity fields you configure (Details, State, artwork keys, hover text and the timestamp) are sent — to the Discord
              app running on this computer, which publishes them to your profile. DeskPresence has no telemetry, accounts or cloud sync.
            </p>
          </div>
        </Card>

        <Card title="Appearance and diagnostics">
          <div className="grid-2">
            <div className="field">
              <span className="field-label">Theme</span>
              <SegmentedControl<Theme>
                label="Theme"
                value={draft.ui.theme}
                onChange={(value) => updateDraft((c) => ({ ...c, ui: { ...c.ui, theme: value } }))}
                options={[
                  { value: "system", label: "System", icon: Monitor },
                  { value: "light", label: "Light", icon: Sun },
                  { value: "dark", label: "Dark", icon: Moon },
                ]}
              />
            </div>
            <Field label="Log detail" htmlFor={ids.log} description="Logs stay on this computer and rotate automatically (about 8 MB maximum).">
              <Select id={ids.log} value={draft.diagnostics.log_level} onChange={(e) => updateDraft((c) => ({ ...c, diagnostics: { log_level: e.target.value as LogLevel } }))}>
                {(Object.keys(logLevelLabel) as LogLevel[]).map((level) => (
                  <option key={level} value={level}>
                    {logLevelLabel[level]}
                  </option>
                ))}
              </Select>
            </Field>
          </div>
        </Card>

        <Card title="Your settings" description="Export a backup, import settings from another computer, or start over.">
          {dirty ? <p className="field-hint warning">You have unsaved changes. Exporting saves the last saved settings.</p> : null}
          <div className="button-row">
            <Button icon={Download} onClick={() => void run("export", async () => { const path = await backend.exportConfig(); if (path) notify("success", `Exported to ${path}`); })} loading={busy === "export"}>
              Export…
            </Button>
            <Button icon={Upload} onClick={() => void startImport()} loading={busy === "import"}>
              Import…
            </Button>
            <Button icon={Sparkles} variant="ghost" onClick={onShowOnboarding}>
              Run setup again
            </Button>
            <span className="spacer" />
            <Button icon={RotateCcw} variant="danger" onClick={() => setConfirmReset(true)}>
              Reset settings…
            </Button>
          </div>
        </Card>

        <Card title="Exit">
          <div className="status-row">
            <div>
              <p className="status-row-title">Quit DeskPresence completely</p>
              <p className="field-description">Clears your Discord presence, stops detection and removes the tray icon.</p>
            </div>
            <Button icon={LogOut} onClick={() => void backend.quit()}>
              Exit DeskPresence
            </Button>
          </div>
        </Card>
      </div>

      {importPreview ? (
        <Dialog
          title="Replace your settings?"
          description={`Importing “${importPreview.file_name}” replaces your current settings. A backup of the current settings is saved first.${importPreview.migrated ? ` The file uses an older format (version ${importPreview.from_version}) and was upgraded.` : ""}`}
          onClose={() => setImportPreview(null)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setImportPreview(null)}>
                Cancel
              </Button>
              <Button variant="primary" onClick={() => void applyImport()} loading={busy === "apply-import"}>
                Replace settings
              </Button>
            </>
          }
        >
          {importPreview.changes.length === 0 ? (
            <p className="muted">The file matches your current settings.</p>
          ) : (
            <ul className="change-list">
              {importPreview.changes.map((change, i) => (
                <li key={i}>
                  <span className="change-section">{change.section}</span>
                  {change.description}
                </li>
              ))}
            </ul>
          )}
          {dirty ? <p className="field-hint warning">Your unsaved changes will be discarded.</p> : null}
        </Dialog>
      ) : null}

      {confirmReset ? (
        <Dialog
          title="Reset all settings?"
          tone="danger"
          description="Templates, rules, exclusions and preferences return to their defaults, and launch at startup is turned off. A backup of your current settings is saved next to the configuration file."
          onClose={() => setConfirmReset(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setConfirmReset(false)}>
                Keep my settings
              </Button>
              <Button variant="danger" onClick={() => void reset()} loading={busy === "reset"}>
                Reset settings
              </Button>
            </>
          }
        />
      ) : null}
    </>
  );
}
