import { useEffect, useState } from "react";
import { Activity, Gauge, LayoutGrid, ListFilter, MessageSquareText, Pause, Play, SlidersHorizontal, type LucideIcon } from "lucide-react";
import { SaveBar } from "./components/SaveBar";
import { Toasts } from "./components/Toasts";
import { Button, StatusDot } from "./components/ui";
import { connectionSummary } from "./lib/labels";
import { ApplicationsPage } from "./pages/ApplicationsPage";
import { DiagnosticsPage } from "./pages/DiagnosticsPage";
import { Onboarding } from "./pages/Onboarding";
import { OverviewPage } from "./pages/OverviewPage";
import { PresencePage } from "./pages/PresencePage";
import { RulesPage } from "./pages/RulesPage";
import { SettingsPage } from "./pages/SettingsPage";
import { useStore } from "./state/store";

export type PageId = "overview" | "applications" | "presence" | "rules" | "settings" | "diagnostics";

const NAV: Array<{ id: PageId; label: string; icon: LucideIcon }> = [
  { id: "overview", label: "Overview", icon: Gauge },
  { id: "applications", label: "Applications", icon: LayoutGrid },
  { id: "presence", label: "Presence", icon: MessageSquareText },
  { id: "rules", label: "Rules", icon: ListFilter },
  { id: "settings", label: "Settings", icon: SlidersHorizontal },
  { id: "diagnostics", label: "Diagnostics", icon: Activity },
];

function useTheme() {
  const { envelope } = useStore();
  const theme = envelope?.config.ui.theme ?? "system";
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") delete root.dataset.theme;
    else root.dataset.theme = theme;
  }, [theme]);
}

export function App() {
  const { backend, view, envelope, draft, loadError, notify } = useStore();
  const [page, setPage] = useState<PageId>("overview");
  const [onboardingOpen, setOnboardingOpen] = useState<boolean | null>(null);
  useTheme();

  // Decide once, when settings first load. Finishing setup closes it explicitly
  // so the final "Ready" step stays visible after saving.
  if (envelope && onboardingOpen === null) {
    setOnboardingOpen(!envelope.config.ui.onboarding_completed);
  }

  if (loadError) {
    return (
      <div className="boot-screen">
        <p className="boot-title">DeskPresence couldn't load its settings</p>
        <p className="boot-text">{loadError}</p>
      </div>
    );
  }
  if (!envelope || !draft || !view) {
    return (
      <div className="boot-screen" aria-busy="true">
        <span className="spinner" aria-hidden />
      </div>
    );
  }
  if (onboardingOpen) {
    return (
      <>
        <Onboarding onDone={() => setOnboardingOpen(false)} />
        <Toasts />
      </>
    );
  }

  const connection = connectionSummary(view.discord);
  const paused = view.presence.paused;
  const togglePause = async () => {
    try {
      await backend.setPaused(!paused);
    } catch {
      notify("error", "Couldn't change the pause state.");
    }
  };

  return (
    <div className="shell">
      <nav className="sidebar" aria-label="Sections">
        <div className="brand">
          <span className="brand-mark" aria-hidden>
            <span className="brand-window" />
            <span className="brand-dot" />
          </span>
          <span className="brand-name">DeskPresence</span>
        </div>
        <ul className="nav-list">
          {NAV.map((item) => {
            const Icon = item.icon;
            return (
              <li key={item.id}>
                <button
                  type="button"
                  className={`nav-item${page === item.id ? " is-active" : ""}`}
                  aria-current={page === item.id ? "page" : undefined}
                  title={item.label}
                  onClick={() => setPage(item.id)}
                >
                  <Icon size={17} aria-hidden />
                  {item.label}
                </button>
              </li>
            );
          })}
        </ul>
        <div className="sidebar-footer">
          <div className="sidebar-status" title={connection.detail}>
            <StatusDot tone={paused ? "neutral" : connection.tone} />
            <span>{paused ? "Presence paused" : connection.label}</span>
          </div>
          <Button size="sm" variant="secondary" icon={paused ? Play : Pause} onClick={() => void togglePause()} className="sidebar-pause">
            {paused ? "Resume" : "Pause"}
          </Button>
        </div>
      </nav>
      <main className="content">
        {backend.kind === "preview" ? (
          <div className="preview-banner" role="note">
            Browser preview — sample data only. Not connected to Windows or Discord.
          </div>
        ) : null}
        <div className="page">
          {page === "overview" && <OverviewPage onNavigate={setPage} />}
          {page === "applications" && <ApplicationsPage />}
          {page === "presence" && <PresencePage />}
          {page === "rules" && <RulesPage />}
          {page === "settings" && <SettingsPage onShowOnboarding={() => setOnboardingOpen(true)} />}
          {page === "diagnostics" && <DiagnosticsPage />}
        </div>
        <SaveBar />
      </main>
      <Toasts />
    </div>
  );
}
