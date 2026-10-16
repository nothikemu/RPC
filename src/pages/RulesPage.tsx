import { useId, useState } from "react";
import { ArrowDown, ArrowUp, ListFilter, Pencil, Plus, Trash } from "lucide-react";
import { AppRuleEditor } from "../components/AppRuleEditor";
import { Badge, Button, Card, EmptyState, Field, IconButton, PageHeader, RadioCards, SegmentedControl, Select, Switch, TextInput } from "../components/ui";
import { newRuleId } from "../lib/format";
import { excludedFocusLabel, matchKindLabel } from "../lib/labels";
import type { AppRule, ExcludedFocusBehavior, MatchKind, SelectionMode } from "../lib/types";
import { clamp, matcherError, PRIORITY } from "../lib/validation";
import { useIssue, useStore } from "../state/store";

export function RulesPage() {
  const { draft, updateDraft } = useStore();
  const [editing, setEditing] = useState<{ rule: AppRule | null } | null>(null);
  const [newKind, setNewKind] = useState<MatchKind>("exe_name");
  const [newValue, setNewValue] = useState("");
  const [newTouched, setNewTouched] = useState(false);
  const ids = { kind: useId(), value: useId() };
  const rulesIssue = useIssue("rules");
  const exclusionsIssue = useIssue("exclusions");

  if (!draft) return null;
  const presence = draft.presence;
  const setPresence = (patch: Partial<typeof presence>) => updateDraft((c) => ({ ...c, presence: { ...c.presence, ...patch } }));
  const setRules = (update: (rules: AppRule[]) => AppRule[]) => updateDraft((c) => ({ ...c, rules: update(c.rules) }));

  const move = (index: number, delta: number) =>
    setRules((rules) => {
      const next = [...rules];
      const target = index + delta;
      if (target < 0 || target >= next.length) return rules;
      const [item] = next.splice(index, 1);
      if (item) next.splice(target, 0, item);
      return next;
    });

  const newError = newTouched ? matcherError(newKind, newValue) : null;
  const duplicate = draft.exclusions.some(
    (x) => x.matcher.kind === newKind && x.matcher.value.trim().toLowerCase() === newValue.trim().toLowerCase(),
  );
  const addExclusion = () => {
    setNewTouched(true);
    if (matcherError(newKind, newValue) || duplicate) return;
    updateDraft((c) => ({
      ...c,
      exclusions: [...c.exclusions, { id: newRuleId("x"), matcher: { kind: newKind, value: newValue.trim() }, note: null }],
    }));
    setNewValue("");
    setNewTouched(false);
  };

  return (
    <>
      <PageHeader title="Rules" description="Decide which application controls your presence when several are open." />
      <div className="stack">
        <Card title="Selection mode">
          <SegmentedControl<SelectionMode>
            label="Selection mode"
            value={presence.selection_mode}
            onChange={(value) => setPresence({ selection_mode: value })}
            options={[
              { value: "foreground", label: "Foreground" },
              { value: "priority", label: "Priority" },
            ]}
          />
          {presence.selection_mode === "foreground" ? (
            <>
              <p className="field-description">
                Shows the application whose window you're using. Brief focus changes (under the debounce time in Settings) are ignored, and
                clicking the taskbar, desktop or DeskPresence itself keeps the current application.
              </p>
              <RadioCards<ExcludedFocusBehavior>
                name="excluded-focus"
                label="When an excluded or disabled application is focused"
                value={presence.excluded_focus}
                onChange={(value) => setPresence({ excluded_focus: value })}
                options={[
                  { value: "keep_previous", label: excludedFocusLabel.keep_previous, description: "The most recently used eligible application stays visible." },
                  { value: "clear", label: excludedFocusLabel.clear, description: "Nothing is shown while you use an excluded application." },
                ]}
              />
            </>
          ) : (
            <>
              <p className="field-description">
                Shows the running application with the highest priority among applications that have an enabled rule. Ties go to the focused
                application, then the most recently focused, then alphabetical path order.
              </p>
              <Switch
                checked={presence.priority_fallback_to_foreground}
                onChange={(value) => setPresence({ priority_fallback_to_foreground: value })}
                label="Fall back to the focused application"
                description="When no prioritized application is running, show the focused application instead of clearing the presence."
              />
            </>
          )}
        </Card>

        <Card title="How rules are applied">
          <ol className="precedence">
            <li>
              <strong>Exclusions always win.</strong> An excluded application is never shown, pinned or offered in the tray.
            </li>
            <li>
              <strong>Pinning</strong> from the tray or the Applications page overrides the selection mode until you unpin or the application
              exits.
            </li>
            <li>
              <strong>One rule per application.</strong> An exact path rule beats an executable-name rule, which beats a pattern. Among
              patterns, the higher one in the list wins.
            </li>
            <li>
              <strong>Disabled rules</strong> keep an application out of the presence but still list it here.
            </li>
            <li>
              <strong>Nothing eligible?</strong> The presence is cleared rather than left stale.
            </li>
          </ol>
        </Card>

        <Card
          title="Application rules"
          description="Order matters only for patterns. Display names never affect matching."
          actions={
            <Button size="sm" icon={Plus} onClick={() => setEditing({ rule: null })}>
              Add rule
            </Button>
          }
        >
          {rulesIssue ? <p className="field-error">{rulesIssue}</p> : null}
          {draft.rules.length === 0 ? (
            <EmptyState icon={ListFilter} title="No application rules">
              Every application uses the global settings. Add a rule to rename an application, change its text or set a priority.
            </EmptyState>
          ) : (
            <ul className="rule-list">
              {draft.rules.map((rule, index) => (
                <li key={rule.id} className={`rule-row${rule.enabled ? "" : " is-disabled"}`}>
                  <div className="rule-order">
                    <IconButton icon={ArrowUp} label="Move up" disabled={index === 0} onClick={() => move(index, -1)} />
                    <IconButton icon={ArrowDown} label="Move down" disabled={index === draft.rules.length - 1} onClick={() => move(index, 1)} />
                  </div>
                  <div className="rule-main">
                    <span className="rule-title">{rule.display_name ?? rule.matcher.value.split(/[\\/]/).pop()}</span>
                    <span className="rule-sub">
                      <Badge>{matchKindLabel[rule.matcher.kind]}</Badge>
                      <span className="mono" title={rule.matcher.value}>
                        {rule.matcher.value}
                      </span>
                    </span>
                  </div>
                  <label className="rule-priority">
                    <span className="sr-only">Priority for {rule.display_name ?? rule.matcher.value}</span>
                    <span aria-hidden>Priority</span>
                    <input
                      className="input input-tiny"
                      type="number"
                      min={PRIORITY.min}
                      max={PRIORITY.max}
                      value={rule.priority}
                      onChange={(e) => {
                        const value = clamp(Number.parseInt(e.target.value || "0", 10), PRIORITY.min, PRIORITY.max);
                        setRules((rules) => rules.map((r) => (r.id === rule.id ? { ...r, priority: value } : r)));
                      }}
                    />
                  </label>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={rule.enabled}
                    aria-label={`${rule.enabled ? "Disable" : "Enable"} ${rule.display_name ?? rule.matcher.value}`}
                    className="switch"
                    onClick={() => setRules((rules) => rules.map((r) => (r.id === rule.id ? { ...r, enabled: !r.enabled } : r)))}
                  >
                    <span className="switch-thumb" />
                  </button>
                  <IconButton icon={Pencil} label="Edit rule" onClick={() => setEditing({ rule })} />
                  <IconButton icon={Trash} label="Delete rule" onClick={() => setRules((rules) => rules.filter((r) => r.id !== rule.id))} />
                </li>
              ))}
            </ul>
          )}
        </Card>

        <Card title="Exclusions" description="Excluded applications never appear in your presence or the tray menu.">
          {exclusionsIssue ? <p className="field-error">{exclusionsIssue}</p> : null}
          {draft.exclusions.length === 0 ? (
            <p className="muted">No exclusions. DeskPresence already ignores its own window and Windows system surfaces.</p>
          ) : (
            <ul className="simple-list">
              {draft.exclusions.map((x) => (
                <li key={x.id} className="simple-row">
                  <Badge>{matchKindLabel[x.matcher.kind]}</Badge>
                  <div className="simple-main">
                    <span className="simple-title mono" title={x.matcher.value}>
                      {x.matcher.value}
                    </span>
                    {x.note ? <span className="simple-sub">{x.note}</span> : null}
                  </div>
                  <IconButton
                    icon={Trash}
                    label={`Remove exclusion ${x.matcher.value}`}
                    onClick={() => updateDraft((c) => ({ ...c, exclusions: c.exclusions.filter((e) => e.id !== x.id) }))}
                  />
                </li>
              ))}
            </ul>
          )}
          <form
            className="add-row"
            onSubmit={(e) => {
              e.preventDefault();
              addExclusion();
            }}
          >
            <Field label="Match by" htmlFor={ids.kind}>
              <Select id={ids.kind} value={newKind} onChange={(e) => setNewKind(e.target.value as MatchKind)}>
                {(["exe_name", "path", "pattern"] as const).map((k) => (
                  <option key={k} value={k}>
                    {matchKindLabel[k]}
                  </option>
                ))}
              </Select>
            </Field>
            <Field
              label="Value"
              htmlFor={ids.value}
              error={newError ?? (duplicate && newTouched ? "This exclusion already exists." : null)}
            >
              <TextInput
                id={ids.value}
                className="mono"
                value={newValue}
                placeholder={newKind === "exe_name" ? "example.exe" : newKind === "path" ? "C:\\Program Files\\App\\app.exe" : "*helper*.exe"}
                onChange={(e) => setNewValue(e.target.value)}
                onBlur={() => newValue && setNewTouched(true)}
                invalid={!!newError}
              />
            </Field>
            <Button type="submit" icon={Plus} className="add-row-button">
              Add exclusion
            </Button>
          </form>
        </Card>
      </div>
      {editing ? <AppRuleEditor app={null} rule={editing.rule} onClose={() => setEditing(null)} /> : null}
    </>
  );
}
