import { useId, useRef } from "react";
import { DISCORD_TEXT_LIMIT } from "../lib/validation";
import { VARIABLES, templateError, type VariableName } from "../lib/templates";

/**
 * Text input for presence templates with variable insertion buttons,
 * inline validation and a length hint for the rendered result.
 */
export function TemplateInput({
  label,
  value,
  onChange,
  placeholder,
  description,
  renderedLength,
  windowTitlesEnabled,
  serverError,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  description?: string;
  renderedLength?: number | null;
  windowTitlesEnabled: boolean;
  serverError?: string | null;
}) {
  const id = useId();
  const ref = useRef<HTMLInputElement>(null);
  const error = templateError(value)?.message ?? serverError ?? null;
  const over = renderedLength != null && renderedLength > DISCORD_TEXT_LIMIT;

  const insert = (name: VariableName) => {
    const input = ref.current;
    const token = `{${name}}`;
    const start = input?.selectionStart ?? value.length;
    const end = input?.selectionEnd ?? value.length;
    const next = value.slice(0, start) + token + value.slice(end);
    onChange(next);
    requestAnimationFrame(() => {
      input?.focus();
      input?.setSelectionRange(start + token.length, start + token.length);
    });
  };

  return (
    <div className={`field template-field${error ? " has-error" : ""}`}>
      <div className="field-head">
        <label className="field-label" htmlFor={id}>
          {label}
        </label>
        {renderedLength != null ? (
          <span className={`field-aside counter${over ? " is-over" : ""}`} aria-live="polite">
            {renderedLength}/{DISCORD_TEXT_LIMIT}
          </span>
        ) : null}
      </div>
      {description ? <p className="field-description">{description}</p> : null}
      <input
        id={id}
        ref={ref}
        className="input mono"
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        aria-invalid={error ? true : undefined}
        onChange={(e) => onChange(e.target.value)}
      />
      <div className="chips" role="group" aria-label={`Insert a variable into ${label}`}>
        {VARIABLES.map((v) => {
          const blocked = v.name === "window_title" && !windowTitlesEnabled;
          return (
            <button
              key={v.name}
              type="button"
              className={`chip${blocked ? " is-muted" : ""}`}
              title={blocked ? `${v.description}. Turn on window-title tracking in Settings → Privacy first.` : v.description}
              onClick={() => insert(v.name)}
            >
              {`{${v.name}}`}
            </button>
          );
        })}
      </div>
      {error ? (
        <p className="field-error" role="alert">
          {error}
        </p>
      ) : over ? (
        <p className="field-hint warning">Longer than Discord's {DISCORD_TEXT_LIMIT}-character limit; it will be shortened with “…”.</p>
      ) : null}
    </div>
  );
}
