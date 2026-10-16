import { useEffect, useId, useState } from "react";
import { Pencil } from "lucide-react";
import { AppRuleEditor } from "../components/AppRuleEditor";
import { PresenceCard } from "../components/PresenceCard";
import { TemplateInput } from "../components/TemplateInput";
import { Badge, Callout, Card, Field, IconButton, PageHeader, RadioCards, Select, Switch, TextInput } from "../components/ui";
import { toCommandError } from "../lib/backend";
import { activityTypeLabel, statusDisplayLabel, timestampModeLabel } from "../lib/labels";
import { discordLength } from "../lib/templates";
import type { Activity, ActivityType, AppRule, PreviewResult, StatusDisplay, TimestampMode } from "../lib/types";
import { assetValueError } from "../lib/validation";
import { useIssue, useStore } from "../state/store";

const ACTIVITY_CODES: Record<ActivityType, number> = { playing: 0, listening: 2, watching: 3, competing: 5 };

function useDebounced<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(value), delayMs);
    return () => window.clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}

export function PresencePage() {
  const { backend, view, draft, envelope, updateDraft } = useStore();
  const [previewApp, setPreviewApp] = useState<string>("");
  const [preview, setPreview] = useState<PreviewResult | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [editingRule, setEditingRule] = useState<AppRule | null>(null);
  // Sample timer start for the preview: 25 minutes ago when the page opened.
  const [previewStart] = useState(() => Math.floor(Date.now() / 1000) - 25 * 60);
  const ids = { app: useId(), type: useId(), display: useId(), large: useId(), largeText: useId(), small: useId(), smallText: useId() };
  const detailsIssue = useIssue("presence.details_template");
  const stateIssue = useIssue("presence.state_template");

  const details = draft?.presence.details_template ?? "";
  const stateTemplate = draft?.presence.state_template ?? "";
  const request = useDebounced({ details, stateTemplate, previewApp, titles: draft?.privacy.capture_window_titles }, 200);

  useEffect(() => {
    let active = true;
    backend
      .previewPresence({
        details_template: request.details,
        state_template: request.stateTemplate,
        app_key: request.previewApp || null,
      })
      .then((result) => {
        if (!active) return;
        setPreview(result);
        setPreviewError(null);
      })
      .catch((error) => active && setPreviewError(toCommandError(error).message));
    return () => {
      active = false;
    };
  }, [backend, request]);

  if (!draft || !view || !envelope) return null;
  const p = draft.presence;
  const setPresence = (patch: Partial<typeof p>) => updateDraft((c) => ({ ...c, presence: { ...c.presence, ...patch } }));
  const setDiscord = (patch: Partial<typeof draft.discord>) => updateDraft((c) => ({ ...c, discord: { ...c.discord, ...patch } }));
  const setAsset = (field: keyof typeof p.default_assets, value: string) =>
    setPresence({ default_assets: { ...p.default_assets, [field]: value.trim() === "" ? null : value } });

  const previewActivity: Activity | null = preview
    ? {
        type: ACTIVITY_CODES[draft.discord.activity_type],
        name: draft.discord.override_activity_name ? preview.app_display_name : undefined,
        details: preview.details ?? undefined,
        state: preview.state ?? undefined,
        timestamps: p.timestamp_mode === "off" ? undefined : { start: previewStart },
        assets:
          p.default_assets.large_image || p.default_assets.small_image
            ? { large_image: p.default_assets.large_image ?? undefined, small_image: p.default_assets.small_image ?? undefined }
            : undefined,
      }
    : null;

  const runningApps = view.apps.filter((a) => a.running);
  const overrides = draft.rules.filter((r) => r.details_template || r.state_template);

  return (
    <>
      <PageHeader title="Presence" description="Choose the text and artwork Discord shows for the detected application." />
      <div className="presence-layout">
        <div className="stack">
          <Card title="Text" description="Variables are replaced with live values. Text over 128 characters is shortened.">
            <div className="stack-sm">
              <TemplateInput
                label="Details (first line)"
                value={details}
                onChange={(value) => setPresence({ details_template: value })}
                placeholder="Using {app}"
                renderedLength={preview?.details ? discordLength(preview.details) : 0}
                windowTitlesEnabled={draft.privacy.capture_window_titles}
                serverError={detailsIssue}
              />
              <TemplateInput
                label="State (second line)"
                value={stateTemplate}
                onChange={(value) => setPresence({ state_template: value })}
                placeholder="Optional, e.g. for {elapsed}"
                renderedLength={preview?.state ? discordLength(preview.state) : 0}
                windowTitlesEnabled={draft.privacy.capture_window_titles}
                serverError={stateIssue}
              />
            </div>
          </Card>

          <Card title="Elapsed time" description="Discord shows a live timer from the timestamp DeskPresence sends.">
            <RadioCards<TimestampMode>
              name="timestamp-mode"
              label="Elapsed time"
              value={p.timestamp_mode}
              onChange={(value) => setPresence({ timestamp_mode: value })}
              options={(Object.keys(timestampModeLabel) as TimestampMode[]).map((value) => ({ value, ...timestampModeLabel[value] }))}
            />
          </Card>

          <Card title="How Discord shows it">
            <div className="grid-2">
              <Field label="Activity type" htmlFor={ids.type} description="The verb Discord shows before your application name.">
                <Select id={ids.type} value={draft.discord.activity_type} onChange={(e) => setDiscord({ activity_type: e.target.value as ActivityType })}>
                  {(Object.keys(activityTypeLabel) as ActivityType[]).map((t) => (
                    <option key={t} value={t}>
                      {activityTypeLabel[t]} …
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Member list status shows" htmlFor={ids.display} description="Which text appears under your name in member lists.">
                <Select id={ids.display} value={draft.discord.status_display} onChange={(e) => setDiscord({ status_display: e.target.value as StatusDisplay })}>
                  {(Object.keys(statusDisplayLabel) as StatusDisplay[]).map((s) => (
                    <option key={s} value={s}>
                      {statusDisplayLabel[s]}
                    </option>
                  ))}
                </Select>
              </Field>
            </div>
            <Switch
              checked={draft.discord.override_activity_name}
              onChange={(value) => setDiscord({ override_activity_name: value })}
              label={
                <>
                  Use the detected app's name as the activity name <Badge tone="warning">Experimental</Badge>
                </>
              }
              description="Asks Discord to show “Playing Zed” instead of your Discord application's name. Discord documents this field but may ignore it for local integrations."
            />
          </Card>

          <Card title="Default artwork" description="Used when an application rule doesn't set its own. Leave empty to show your Discord application's icon.">
            <div className="grid-2">
              <Field label="Large image" htmlFor={ids.large} description="Asset key or https:// URL" error={assetValueError(p.default_assets.large_image ?? "")}>
                <TextInput id={ids.large} className="mono" value={p.default_assets.large_image ?? ""} onChange={(e) => setAsset("large_image", e.target.value)} />
              </Field>
              <Field label="Large image hover text" htmlFor={ids.largeText} description="Supports variables">
                <TextInput id={ids.largeText} value={p.default_assets.large_text ?? ""} onChange={(e) => setAsset("large_text", e.target.value)} />
              </Field>
              <Field label="Small image" htmlFor={ids.small} error={assetValueError(p.default_assets.small_image ?? "")}>
                <TextInput id={ids.small} className="mono" value={p.default_assets.small_image ?? ""} onChange={(e) => setAsset("small_image", e.target.value)} />
              </Field>
              <Field label="Small image hover text" htmlFor={ids.smallText}>
                <TextInput id={ids.smallText} value={p.default_assets.small_text ?? ""} onChange={(e) => setAsset("small_text", e.target.value)} />
              </Field>
            </div>
          </Card>

          <Card title="Per-application overrides" description="Applications with their own Details or State text. Edit any application from the Applications page.">
            {overrides.length === 0 ? (
              <p className="muted">No overrides yet.</p>
            ) : (
              <ul className="simple-list">
                {overrides.map((rule) => (
                  <li key={rule.id} className="simple-row">
                    <div className="simple-main">
                      <span className="simple-title">{rule.display_name ?? rule.matcher.value}</span>
                      <span className="simple-sub mono">
                        {rule.details_template ?? "(global details)"} · {rule.state_template ?? "(global state)"}
                      </span>
                    </div>
                    <IconButton icon={Pencil} label={`Edit ${rule.display_name ?? rule.matcher.value}`} onClick={() => setEditingRule(rule)} />
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>

        <aside className="preview-column">
          <Card title="Live preview">
            <Field label="Preview with" htmlFor={ids.app}>
              <Select id={ids.app} value={previewApp} onChange={(e) => setPreviewApp(e.target.value)}>
                <option value="">Current application</option>
                {runningApps.map((a) => (
                  <option key={a.key} value={a.key}>
                    {a.display_name}
                  </option>
                ))}
              </Select>
            </Field>
            <PresenceCard activity={previewActivity} emptyText="Fix the template errors to see a preview." compact />
            {preview?.sample ? <p className="field-hint">No application is running, so sample values are used.</p> : null}
            {preview?.warnings.map((w, i) => (
              <p key={i} className="field-hint warning">
                {w.message}
              </p>
            ))}
            {previewError ? <Callout tone="error">{previewError}</Callout> : null}
          </Card>
        </aside>
      </div>
      {editingRule ? <AppRuleEditor app={null} rule={editingRule} onClose={() => setEditingRule(null)} /> : null}
    </>
  );
}
