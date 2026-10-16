import { useId, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from "react";
import { CircleAlert, CircleCheck, Info, TriangleAlert, type LucideIcon } from "lucide-react";
import type { Tone } from "../lib/labels";

type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export function Button({
  variant = "secondary",
  size = "md",
  icon: Icon,
  loading = false,
  children,
  className,
  disabled,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: "sm" | "md";
  icon?: LucideIcon;
  loading?: boolean;
}) {
  return (
    <button
      type="button"
      className={`btn btn-${variant} btn-${size}${className ? ` ${className}` : ""}`}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading ? <span className="spinner" aria-hidden /> : Icon ? <Icon size={16} aria-hidden /> : null}
      {children}
    </button>
  );
}

/** Square icon-only button; `label` is required and used for the tooltip and accessible name. */
export function IconButton({
  icon: Icon,
  label,
  className,
  active,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { icon: LucideIcon; label: string; active?: boolean }) {
  return (
    <button
      type="button"
      className={`icon-btn${active ? " is-active" : ""}${className ? ` ${className}` : ""}`}
      aria-label={label}
      title={label}
      aria-pressed={active}
      {...rest}
    >
      <Icon size={16} aria-hidden />
    </button>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  description,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
}) {
  const generated = useId();
  const inputId = id ?? generated;
  const descriptionId = `${inputId}-description`;
  return (
    <div className={`switch-row${disabled ? " is-disabled" : ""}`}>
      <div className="switch-text">
        <label htmlFor={inputId} className="switch-label">
          {label}
        </label>
        {description ? (
          <p id={descriptionId} className="field-description">
            {description}
          </p>
        ) : null}
      </div>
      <button
        id={inputId}
        type="button"
        role="switch"
        aria-checked={checked}
        aria-describedby={description ? descriptionId : undefined}
        className="switch"
        disabled={disabled}
        onClick={() => onChange(!checked)}
      >
        <span className="switch-thumb" />
      </button>
    </div>
  );
}

export function Field({
  label,
  description,
  error,
  children,
  htmlFor,
  aside,
}: {
  label: ReactNode;
  description?: ReactNode;
  error?: string | null;
  children: ReactNode;
  htmlFor: string;
  aside?: ReactNode;
}) {
  return (
    <div className={`field${error ? " has-error" : ""}`}>
      <div className="field-head">
        <label className="field-label" htmlFor={htmlFor}>
          {label}
        </label>
        {aside ? <span className="field-aside">{aside}</span> : null}
      </div>
      {description ? <p className="field-description">{description}</p> : null}
      {children}
      {error ? (
        <p className="field-error" role="alert">
          <CircleAlert size={14} aria-hidden />
          {error}
        </p>
      ) : null}
    </div>
  );
}

export function TextInput({ invalid, className, ...rest }: InputHTMLAttributes<HTMLInputElement> & { invalid?: boolean }) {
  return <input className={`input${className ? ` ${className}` : ""}`} aria-invalid={invalid || undefined} {...rest} />;
}

export function Select({ className, children, ...rest }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select className={`input select${className ? ` ${className}` : ""}`} {...rest}>
      {children}
    </select>
  );
}

export function SegmentedControl<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: Array<{ value: T; label: string; icon?: LucideIcon }>;
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div className="segmented" role="radiogroup" aria-label={label}>
      {options.map((option) => {
        const Icon = option.icon;
        const selected = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            className={`segment${selected ? " is-selected" : ""}`}
            onClick={() => onChange(option.value)}
          >
            {Icon ? <Icon size={14} aria-hidden /> : null}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}

export function RadioCards<T extends string>({
  value,
  options,
  onChange,
  name,
  label,
}: {
  value: T;
  options: Array<{ value: T; label: string; description?: string }>;
  onChange: (value: T) => void;
  name: string;
  label: string;
}) {
  return (
    <fieldset className="radio-cards">
      <legend className="sr-only">{label}</legend>
      {options.map((option) => (
        <label key={option.value} className={`radio-card${option.value === value ? " is-selected" : ""}`}>
          <input
            type="radio"
            name={name}
            value={option.value}
            checked={option.value === value}
            onChange={() => onChange(option.value)}
          />
          <span className="radio-card-text">
            <span className="radio-card-label">{option.label}</span>
            {option.description ? <span className="radio-card-description">{option.description}</span> : null}
          </span>
        </label>
      ))}
    </fieldset>
  );
}

export function Badge({ tone = "neutral", children, title }: { tone?: Tone; children: ReactNode; title?: string }) {
  return (
    <span className={`badge badge-${tone}`} title={title}>
      {children}
    </span>
  );
}

export function StatusDot({ tone }: { tone: Tone }) {
  return <span className={`status-dot dot-${tone}`} aria-hidden />;
}

export function Card({ children, className, title, actions, description }: { children: ReactNode; className?: string; title?: ReactNode; actions?: ReactNode; description?: ReactNode }) {
  return (
    <section className={`card${className ? ` ${className}` : ""}`}>
      {title || actions ? (
        <header className="card-head">
          <div>
            {title ? <h2 className="card-title">{title}</h2> : null}
            {description ? <p className="card-description">{description}</p> : null}
          </div>
          {actions ? <div className="card-actions">{actions}</div> : null}
        </header>
      ) : null}
      {children}
    </section>
  );
}

const calloutIcons: Record<"info" | "warning" | "error" | "success", LucideIcon> = {
  info: Info,
  warning: TriangleAlert,
  error: CircleAlert,
  success: CircleCheck,
};

export function Callout({
  tone = "info",
  title,
  children,
  actions,
}: {
  tone?: "info" | "warning" | "error" | "success";
  title?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  const Icon = calloutIcons[tone];
  return (
    <div className={`callout callout-${tone}`} role={tone === "error" ? "alert" : "status"}>
      <Icon size={18} className="callout-icon" aria-hidden />
      <div className="callout-body">
        {title ? <p className="callout-title">{title}</p> : null}
        {children ? <div className="callout-text">{children}</div> : null}
      </div>
      {actions ? <div className="callout-actions">{actions}</div> : null}
    </div>
  );
}

export function EmptyState({ icon: Icon, title, children, action }: { icon: LucideIcon; title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty-state">
      <Icon size={22} aria-hidden className="empty-icon" />
      <p className="empty-title">{title}</p>
      {children ? <p className="empty-text">{children}</p> : null}
      {action}
    </div>
  );
}

export function PageHeader({ title, description, actions }: { title: string; description?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="page-header">
      <div>
        <h1 className="page-title">{title}</h1>
        {description ? <p className="page-description">{description}</p> : null}
      </div>
      {actions ? <div className="page-actions">{actions}</div> : null}
    </header>
  );
}

export function KeyValue({ items }: { items: Array<{ label: string; value: ReactNode }> }) {
  return (
    <dl className="kv">
      {items.map((item) => (
        <div className="kv-row" key={item.label}>
          <dt>{item.label}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}
