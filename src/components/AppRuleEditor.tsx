import { useId, useState } from "react";
import { Trash } from "lucide-react";
import { newRuleId } from "../lib/format";
import { matchKindLabel } from "../lib/labels";
import type { AppRule, AppView, MatchKind } from "../lib/types";
import { assetValueError, clamp, matcherError, PRIORITY } from "../lib/validation";
import { useStore } from "../state/store";
import { Dialog, Drawer } from "./Overlay";
import { TemplateInput } from "./TemplateInput";
import { Button, Field, SegmentedControl, Switch, TextInput } from "./ui";

function emptyToNull(value: string): string | null {
  return value.trim() === "" ? null : value;
}

/** Slide-over editor for one application rule (new or existing). */
export function AppRuleEditor({ app, rule, onClose }: { app: AppView | null; rule: AppRule | null; onClose: () => void }) {
  const { envelope, quickSave, notify } = useStore();
  const ids = { value: useId(), name: useId(), priority: useId(), large: useId(), largeText: useId(), small: useId(), smallText: useId() };
  const config = envelope?.config;
  const initialKind: MatchKind = rule?.matcher.kind ?? (app ? "path" : "exe_name");
  const [kind, setKind] = useState<MatchKind>(initialKind);
  const [value, setValue] = useState(rule?.matcher.value ?? app?.exe_path ?? "");
  const [displayName, setDisplayName] = useState(rule?.display_name ?? "");
  const [enabled, setEnabled] = useState(rule?.enabled ?? true);
  const [priority, setPriority] = useState(String(rule?.priority ?? 0));
  const [details, setDetails] = useState(rule?.details_template ?? "");
  const [stateText, setStateText] = useState(rule?.state_template ?? "");
  const [assets, setAssets] = useState({
    large_image: rule?.assets.large_image ?? "",
    large_text: rule?.assets.large_text ?? "",
    small_image: rule?.assets.small_image ?? "",
    small_text: rule?.assets.small_text ?? "",
  });
  const [touched, setTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  if (!config) return null;

  const switchKind = (next: MatchKind) => {
    setKind(next);
    if (app) setValue(next === "exe_name" ? app.exe_name : next === "path" ? app.exe_path : value);
  };

  const valueError = touched ? matcherError(kind, value) : null;
  const duplicate = config.rules.some(
    (r) => r.id !== rule?.id && r.matcher.kind === kind && r.matcher.value.trim().toLowerCase() === value.trim().toLowerCase(),
  );
  const priorityNumber = Number.parseInt(priority, 10);
  const priorityError = Number.isNaN(priorityNumber) ? "Enter a whole number." : null;
  const largeError = assetValueError(assets.large_image);
  const smallError = assetValueError(assets.small_image);

  const save = async () => {
    setTouched(true);
    if (matcherError(kind, value) || duplicate || priorityError || largeError || smallError) return;
    const next: AppRule = {
      id: rule?.id ?? newRuleId("r"),
      matcher: { kind, value: value.trim() },
      display_name: emptyToNull(displayName),
      enabled,
      priority: clamp(priorityNumber, PRIORITY.min, PRIORITY.max),
      details_template: emptyToNull(details),
      state_template: emptyToNull(stateText),
      assets: {
        large_image: emptyToNull(assets.large_image),
        large_text: emptyToNull(assets.large_text),
        small_image: emptyToNull(assets.small_image),
        small_text: emptyToNull(assets.small_text),
      },
    };
    setSaving(true);
    const ok = await quickSave((c) => ({
      ...c,
      rules: rule ? c.rules.map((r) => (r.id === rule.id ? next : r)) : [...c.rules, next],
    }));
    setSaving(false);
    if (ok) {
      notify("success", "Rule saved.");
      onClose();
    }
  };

  const remove = async () => {
    if (!rule) return;
    const ok = await quickSave((c) => ({ ...c, rules: c.rules.filter((r) => r.id !== rule.id) }));
    if (ok) {
      notify("success", "Rule deleted.");
      onClose();
    }
  };

  const windowTitles = config.privacy.capture_window_titles;
  return (
    <Drawer
      title={rule ? "Edit application rule" : app ? `Customize ${app.display_name}` : "Add application rule"}
      subtitle={app ? app.exe_path : "Rules match by path or executable name, never by display name."}
      onClose={onClose}
      footer={
        <>
          {rule ? (
            <Button variant="danger" icon={Trash} onClick={() => setConfirmDelete(true)}>
              Delete rule
            </Button>
          ) : null}
          <span className="spacer" />
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void save()} loading={saving}>
            Save rule
          </Button>
        </>
      }
    >
      <div className="form-section">
        <h3 className="form-section-title">Match</h3>
        <SegmentedControl<MatchKind>
          label="Match by"
          value={kind}
          onChange={switchKind}
          options={(["path", "exe_name", "pattern"] as const).map((k) => ({ value: k, label: matchKindLabel[k] }))}
        />
        <Field
          label={kind === "path" ? "Executable path" : kind === "exe_name" ? "Executable name" : "Pattern"}
          htmlFor={ids.value}
          description={
            kind === "pattern"
              ? "Use * and ? as wildcards. Patterns containing \\ match the full path; others match the file name."
              : kind === "exe_name"
                ? "Matches this executable wherever it is installed."
                : "Matches only this exact file. Most specific; takes precedence over other rules."
          }
          error={valueError ?? (duplicate ? "Another rule already targets this value." : null)}
        >
          <TextInput
            id={ids.value}
            className="mono"
            value={value}
            invalid={!!valueError || duplicate}
            onChange={(e) => setValue(e.target.value)}
            onBlur={() => setTouched(true)}
            spellCheck={false}
          />
        </Field>
      </div>

      <div className="form-section">
        <h3 className="form-section-title">Behavior</h3>
        <Field
          label="Display name"
          htmlFor={ids.name}
          description="Shown in DeskPresence and in {app}. Leave empty to use the executable's own name."
        >
          <TextInput id={ids.name} value={displayName} placeholder={app?.display_name ?? "Automatic"} onChange={(e) => setDisplayName(e.target.value)} maxLength={128} />
        </Field>
        <Switch
          checked={enabled}
          onChange={setEnabled}
          label="Can control the presence"
          description="When off, this application is never shown automatically or offered in the tray."
        />
        <Field label="Priority" htmlFor={ids.priority} description="Used in priority mode. Higher numbers win." error={priorityError}>
          <TextInput
            id={ids.priority}
            type="number"
            inputMode="numeric"
            min={PRIORITY.min}
            max={PRIORITY.max}
            step={1}
            value={priority}
            onChange={(e) => setPriority(e.target.value)}
            className="input-narrow"
          />
        </Field>
      </div>

      <div className="form-section">
        <h3 className="form-section-title">Text overrides</h3>
        <TemplateInput
          label="Details"
          value={details}
          onChange={setDetails}
          placeholder={`Global: ${config.presence.details_template || "(empty)"}`}
          windowTitlesEnabled={windowTitles}
        />
        <TemplateInput
          label="State"
          value={stateText}
          onChange={setStateText}
          placeholder={`Global: ${config.presence.state_template || "(empty)"}`}
          windowTitlesEnabled={windowTitles}
        />
      </div>

      <div className="form-section">
        <h3 className="form-section-title">Artwork</h3>
        <p className="field-description">
          Use the name of an asset uploaded to your Discord application (Developer Portal → Rich Presence → Art Assets) or an https://
          image URL. Local icons can't be sent to Discord.
        </p>
        <div className="grid-2">
          <Field label="Large image" htmlFor={ids.large} error={largeError}>
            <TextInput id={ids.large} className="mono" value={assets.large_image} placeholder={config.presence.default_assets.large_image ?? "Default"} onChange={(e) => setAssets({ ...assets, large_image: e.target.value })} />
          </Field>
          <Field label="Large image hover text" htmlFor={ids.largeText}>
            <TextInput id={ids.largeText} value={assets.large_text} onChange={(e) => setAssets({ ...assets, large_text: e.target.value })} />
          </Field>
          <Field label="Small image" htmlFor={ids.small} error={smallError}>
            <TextInput id={ids.small} className="mono" value={assets.small_image} placeholder={config.presence.default_assets.small_image ?? "Default"} onChange={(e) => setAssets({ ...assets, small_image: e.target.value })} />
          </Field>
          <Field label="Small image hover text" htmlFor={ids.smallText}>
            <TextInput id={ids.smallText} value={assets.small_text} onChange={(e) => setAssets({ ...assets, small_text: e.target.value })} />
          </Field>
        </div>
      </div>

      {confirmDelete ? (
        <Dialog
          title="Delete this rule?"
          tone="danger"
          description={`The custom settings for “${rule?.display_name ?? rule?.matcher.value}” will be removed. The application itself is not affected.`}
          onClose={() => setConfirmDelete(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setConfirmDelete(false)}>
                Keep rule
              </Button>
              <Button variant="danger" onClick={() => void remove()}>
                Delete rule
              </Button>
            </>
          }
        />
      ) : null}
    </Drawer>
  );
}
