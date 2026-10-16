import { useId, useState } from "react";
import { AppWindow, ArrowLeft, ArrowRight, Check, ExternalLink, Lock, MonitorSmartphone, PanelBottom } from "lucide-react";
import { Badge, Button, Callout, Field, StatusDot, Switch, TextInput } from "../components/ui";
import { toCommandError } from "../lib/backend";
import { connectionSummary } from "../lib/labels";
import { applicationIdError, isValidApplicationId } from "../lib/validation";
import { useStore } from "../state/store";

const STEPS = ["Welcome", "Tray", "Discord", "Startup", "Privacy", "Ready"] as const;

/** First-run setup. Works whether or not Discord is installed or running. */
export function Onboarding({ onDone }: { onDone: () => void }) {
  const { backend, view, envelope, replaceEnvelope, discardDraft, notify } = useStore();
  const config = envelope?.config;
  const [step, setStep] = useState(0);
  const [appId, setAppId] = useState(config?.discord.application_id ?? "");
  const [appIdTouched, setAppIdTouched] = useState(false);
  const [launchAtLogin, setLaunchAtLogin] = useState(config?.startup.launch_at_login ?? false);
  const [windowTitles, setWindowTitles] = useState(config?.privacy.capture_window_titles ?? false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const appIdInput = useId();

  if (!config || !envelope || !view) return null;
  const trimmedId = appId.trim();
  const idError = applicationIdError(trimmedId);
  const hasBuildDefault = view.application_id.source === "build_default";

  const save = async () => {
    setSaving(true);
    try {
      const next = await backend.saveConfig(
        {
          ...config,
          discord: { ...config.discord, application_id: trimmedId === "" ? null : trimmedId },
          startup: { ...config.startup, launch_at_login: launchAtLogin },
          privacy: { capture_window_titles: windowTitles },
          ui: { ...config.ui, onboarding_completed: true },
        },
        envelope.revision,
      );
      discardDraft();
      replaceEnvelope(next);
      setSaved(true);
      setStep(STEPS.length - 1);
    } catch (error) {
      notify("error", toCommandError(error).message);
    } finally {
      setSaving(false);
    }
  };

  const next = () => {
    if (step === 2 && trimmedId !== "" && !isValidApplicationId(trimmedId)) {
      setAppIdTouched(true);
      return;
    }
    if (step === STEPS.length - 2) {
      void save();
      return;
    }
    setStep((s) => Math.min(s + 1, STEPS.length - 1));
  };

  const connection = connectionSummary(view.discord);

  return (
    <div className="onboarding">
      <div className="onboarding-card" role="dialog" aria-modal="true" aria-labelledby="onboarding-title">
        <ol className="stepper" aria-label="Setup progress">
          {STEPS.map((label, i) => (
            <li key={label} className={i === step ? "is-current" : i < step ? "is-done" : ""} aria-current={i === step ? "step" : undefined}>
              <span className="stepper-dot">{i < step ? <Check size={12} aria-hidden /> : i + 1}</span>
              <span className="stepper-label">{label}</span>
            </li>
          ))}
        </ol>

        <div className="onboarding-body">
          {step === 0 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                Show what you're working on, automatically
              </h1>
              <p className="onboarding-text">
                DeskPresence notices which application you're using and shows it as your Discord Rich Presence — for example “Using Zed” with a
                live timer. You choose the wording, which apps appear, and which never do.
              </p>
              <ul className="feature-list">
                <li>
                  <AppWindow size={18} aria-hidden /> Detects applications with visible windows, with their names and icons.
                </li>
                <li>
                  <MonitorSmartphone size={18} aria-hidden /> Updates Discord through its local connection on this computer.
                </li>
                <li>
                  <Lock size={18} aria-hidden /> No accounts, no telemetry, nothing uploaded by DeskPresence itself.
                </li>
              </ul>
            </>
          )}

          {step === 1 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                It lives in the system tray
              </h1>
              <p className="onboarding-text">
                After setup, DeskPresence runs quietly in the notification area near the clock. Closing this window keeps it running.
              </p>
              <ul className="feature-list">
                <li>
                  <PanelBottom size={18} aria-hidden /> <span><strong>Right-click</strong> the tray icon to see detected applications. Click one to pin it as your presence; click again to return to automatic.</span>
                </li>
                <li>
                  <PanelBottom size={18} aria-hidden /> <span><strong>Configure</strong> opens this window. <strong>Exit</strong> clears your presence and quits completely.</span>
                </li>
              </ul>
              <p className="field-description">Tip: if the icon is hidden, drag it from the “^” overflow area onto the taskbar.</p>
            </>
          )}

          {step === 2 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                Connect a Discord application
              </h1>
              <p className="onboarding-text">
                Discord shows Rich Presence under a <strong>Discord application</strong> that you create once in the Developer Portal. Its
                name becomes the top line (“Playing <em>DeskPresence</em>”) and you can upload artwork there. The application you're using on
                Windows appears in the text lines below it.
              </p>
              <ol className="howto">
                <li>
                  Open the Developer Portal and create an application. Name it what you want people to see.{" "}
                  <button type="button" className="link-btn" onClick={() => void backend.openExternal("developer_portal")}>
                    Open Developer Portal <ExternalLink size={13} aria-hidden />
                  </button>
                </li>
                <li>Copy the Application ID from General Information and paste it below. No secret or token is needed.</li>
              </ol>
              <Field
                label="Application ID"
                htmlFor={appIdInput}
                description={hasBuildDefault ? "Optional: this build already includes a default ID." : "You can also add this later in Settings."}
                error={appIdTouched ? idError : null}
              >
                <TextInput
                  id={appIdInput}
                  className="mono input-medium"
                  inputMode="numeric"
                  placeholder="123456789012345678"
                  value={appId}
                  invalid={appIdTouched && !!idError}
                  onChange={(e) => setAppId(e.target.value)}
                  onBlur={() => setAppIdTouched(true)}
                />
              </Field>
              {trimmedId && !idError ? (
                <p className="field-hint">
                  <Check size={14} aria-hidden /> The format looks right. Whether Discord accepts it is checked when DeskPresence connects.
                </p>
              ) : null}
            </>
          )}

          {step === 3 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                Start with Windows?
              </h1>
              <p className="onboarding-text">Set it up once and forget about it: DeskPresence can start in the tray when you sign in.</p>
              <Switch
                checked={launchAtLogin}
                onChange={setLaunchAtLogin}
                label="Launch DeskPresence at Windows startup"
                description="You can change this in Settings or in Task Manager → Startup apps."
              />
            </>
          )}

          {step === 4 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                Your privacy
              </h1>
              <ul className="feature-list">
                <li>
                  <Lock size={18} aria-hidden /> Application names, executable names and icons are read locally and stay on this computer.
                </li>
                <li>
                  <MonitorSmartphone size={18} aria-hidden /> Only the presence text and artwork keys you configure are passed to the Discord
                  app, which shows them on your profile.
                </li>
              </ul>
              <Switch
                checked={windowTitles}
                onChange={setWindowTitles}
                label="Allow window titles in presence text"
                description="Off by default. Window titles can include document names or web page titles. Needed only if you use {window_title}."
              />
            </>
          )}

          {step === 5 && (
            <>
              <h1 id="onboarding-title" className="onboarding-title">
                {saved ? "You're all set" : "Almost done"}
              </h1>
              <div className="ready-grid">
                <div className="ready-item">
                  <p className="tile-label">Discord</p>
                  <p className="tile-value">
                    <StatusDot tone={connection.tone} /> {connection.label}
                  </p>
                  <p className="tile-detail">{connection.detail}</p>
                </div>
                <div className="ready-item">
                  <p className="tile-label">Detected applications</p>
                  <p className="tile-value tile-number">{view.detector.running_count}</p>
                  <p className="tile-detail">
                    {view.presence.selected ? (
                      <>
                        Showing <Badge tone="accent">{view.presence.selected.display_name}</Badge>
                      </>
                    ) : (
                      "Focus an application to see it in your presence."
                    )}
                  </p>
                </div>
              </div>
              {view.discord.phase === "not_configured" ? (
                <Callout tone="info">Without an Application ID, DeskPresence previews your presence but doesn't send it to Discord.</Callout>
              ) : null}
            </>
          )}
        </div>

        <footer className="onboarding-footer">
          {step > 0 && step < STEPS.length - 1 ? (
            <Button variant="ghost" icon={ArrowLeft} onClick={() => setStep((s) => s - 1)}>
              Back
            </Button>
          ) : (
            <span />
          )}
          {step < STEPS.length - 1 ? (
            <Button variant="primary" onClick={next} loading={saving}>
              {step === STEPS.length - 2 ? "Finish setup" : step === 2 && trimmedId === "" ? "Skip for now" : "Continue"}
              {step < STEPS.length - 2 ? <ArrowRight size={16} aria-hidden /> : null}
            </Button>
          ) : (
            <Button variant="primary" onClick={onDone}>
              Open DeskPresence
            </Button>
          )}
        </footer>
      </div>
    </div>
  );
}
