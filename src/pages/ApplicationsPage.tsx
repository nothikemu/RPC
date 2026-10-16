import { useMemo, useState } from "react";
import { Eye, EyeOff, LayoutGrid, Pencil, Pin, PinOff, Plus, RefreshCw, Search } from "lucide-react";
import { AppIcon } from "../components/AppIcon";
import { AppRuleEditor } from "../components/AppRuleEditor";
import { Badge, Button, EmptyState, IconButton, PageHeader, SegmentedControl, TextInput } from "../components/ui";
import { useNow } from "../components/useNow";
import { formatRelative, newRuleId } from "../lib/format";
import { ruleStatusLabel } from "../lib/labels";
import type { AppRule, AppView } from "../lib/types";
import { useStore } from "../state/store";

type Filter = "all" | "running" | "configured" | "excluded";

export function ApplicationsPage() {
  const { backend, view, envelope, quickSave, notify } = useStore();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [editing, setEditing] = useState<{ app: AppView | null; rule: AppRule | null } | null>(null);
  const now = useNow(30_000);

  const apps = useMemo(() => {
    const all = view?.apps ?? [];
    const q = query.trim().toLowerCase();
    return all.filter((a) => {
      if (filter === "running" && !a.running) return false;
      if (filter === "configured" && a.status !== "configured" && a.status !== "disabled") return false;
      if (filter === "excluded" && a.status !== "excluded") return false;
      if (!q) return true;
      return a.display_name.toLowerCase().includes(q) || a.exe_name.toLowerCase().includes(q) || a.exe_path.toLowerCase().includes(q);
    });
  }, [view, query, filter]);

  if (!view || !envelope) return null;
  const config = envelope.config;

  const openEditor = (app: AppView) => {
    const rule = app.rule_id ? config.rules.find((r) => r.id === app.rule_id) ?? null : null;
    setEditing({ app, rule });
  };

  const toggleExclusion = async (app: AppView) => {
    if (app.status === "excluded") {
      const exclusion = config.exclusions.find((x) => x.id === app.exclusion_id);
      if (exclusion && exclusion.matcher.kind === "pattern") {
        notify("info", `Excluded by the pattern “${exclusion.matcher.value}”. Edit it on the Rules page.`);
        return;
      }
      const ok = await quickSave((c) => ({ ...c, exclusions: c.exclusions.filter((x) => x.id !== app.exclusion_id) }));
      if (ok) notify("success", `${app.display_name} can appear in your presence again.`);
    } else {
      const ok = await quickSave((c) => ({
        ...c,
        exclusions: [...c.exclusions, { id: newRuleId("x"), matcher: { kind: "path", value: app.exe_path }, note: app.display_name }],
      }));
      if (ok) notify("success", `${app.display_name} is excluded and will never be shown.`);
    }
  };

  const togglePin = async (app: AppView) => {
    try {
      await backend.togglePin(app.key);
    } catch {
      notify("error", "Couldn't change the pin.");
    }
  };

  const counts = {
    all: view.apps.length,
    running: view.apps.filter((a) => a.running).length,
  };

  return (
    <>
      <PageHeader
        title="Applications"
        description="Applications with visible windows, detected automatically. DeskPresence never reads what's inside them."
        actions={
          <>
            <IconButton icon={RefreshCw} label="Scan now" onClick={() => void backend.rescanNow()} />
            <Button variant="primary" icon={Plus} onClick={() => setEditing({ app: null, rule: null })}>
              Add rule
            </Button>
          </>
        }
      />
      <div className="toolbar">
        <div className="search">
          <Search size={16} aria-hidden className="search-icon" />
          <TextInput
            type="search"
            placeholder="Search by name or executable"
            aria-label="Search applications"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <SegmentedControl<Filter>
          label="Filter applications"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: `All (${counts.all})` },
            { value: "running", label: `Running (${counts.running})` },
            { value: "configured", label: "Custom" },
            { value: "excluded", label: "Excluded" },
          ]}
        />
      </div>

      {apps.length === 0 ? (
        <EmptyState
          icon={LayoutGrid}
          title={query || filter !== "all" ? "No matching applications" : "No applications detected yet"}
          action={
            query || filter !== "all" ? (
              <Button size="sm" onClick={() => { setQuery(""); setFilter("all"); }}>
                Clear filters
              </Button>
            ) : undefined
          }
        >
          {query || filter !== "all"
            ? "Try a different search or filter."
            : "Open an application with a window. It appears here within a second."}
        </EmptyState>
      ) : (
        <ul className="app-list" aria-label="Applications">
          {apps.map((app) => {
            const status = ruleStatusLabel[app.status];
            const eligible = app.status === "automatic" || app.status === "configured";
            return (
              <li key={app.key} className={`app-row${app.running ? "" : " is-stopped"}`}>
                <AppIcon backend={backend} appKey={app.key} running={app.running} />
                <div className="app-main">
                  <div className="app-title">
                    <span className="app-name">{app.display_name}</span>
                    {app.is_selected ? <Badge tone="success">Showing</Badge> : null}
                    {app.is_pinned ? <Badge tone="accent">Pinned</Badge> : null}
                  </div>
                  <span className="app-path" title={app.exe_path}>
                    {app.exe_name}
                    {app.running
                      ? app.window_count > 1
                        ? ` · ${app.window_count} windows`
                        : ""
                      : app.last_seen_ms
                        ? ` · last seen ${formatRelative(app.last_seen_ms, now)}`
                        : app.path_exists === false
                          ? " · path not found"
                          : " · not running"}
                  </span>
                </div>
                <div className="app-meta">
                  <Badge tone={status.tone}>{status.label}</Badge>
                  {app.priority !== 0 ? <span className="app-priority" title="Priority">P{app.priority}</span> : null}
                </div>
                <div className="app-actions">
                  {app.running && eligible ? (
                    <IconButton
                      icon={app.is_pinned ? PinOff : Pin}
                      label={app.is_pinned ? `Unpin ${app.display_name}` : `Pin ${app.display_name} as the presence`}
                      active={app.is_pinned}
                      onClick={() => void togglePin(app)}
                    />
                  ) : null}
                  <IconButton icon={Pencil} label={`Edit ${app.display_name}`} onClick={() => openEditor(app)} />
                  <IconButton
                    icon={app.status === "excluded" ? Eye : EyeOff}
                    label={app.status === "excluded" ? `Stop excluding ${app.display_name}` : `Exclude ${app.display_name}`}
                    onClick={() => void toggleExclusion(app)}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}

      {editing ? <AppRuleEditor app={editing.app} rule={editing.rule} onClose={() => setEditing(null)} /> : null}
    </>
  );
}
