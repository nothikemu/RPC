import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { toCommandError, type Backend } from "../lib/backend";
import type { AppInfo, CommandError, Config, ConfigEnvelope, RuntimeView } from "../lib/types";

export interface Toast {
  id: number;
  tone: "success" | "error" | "info";
  message: string;
}

interface StoreValue {
  backend: Backend;
  info: AppInfo | null;
  view: RuntimeView | null;
  /** The saved configuration. */
  envelope: ConfigEnvelope | null;
  /** Unsaved edits shared by all pages. */
  draft: Config | null;
  dirty: boolean;
  saving: boolean;
  saveError: CommandError | null;
  loadError: string | null;
  updateDraft: (update: (config: Config) => Config) => void;
  saveDraft: () => Promise<boolean>;
  discardDraft: () => void;
  /** Applies a change immediately (also to any unsaved draft). */
  quickSave: (mutate: (config: Config) => Config) => Promise<boolean>;
  replaceEnvelope: (envelope: ConfigEnvelope) => void;
  toasts: Toast[];
  notify: (tone: Toast["tone"], message: string) => void;
  dismissToast: (id: number) => void;
}

const StoreContext = createContext<StoreValue | null>(null);

function sameConfig(a: Config | null, b: Config | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function StoreProvider({ backend, children }: { backend: Backend; children: ReactNode }) {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [view, setView] = useState<RuntimeView | null>(null);
  const [envelope, setEnvelope] = useState<ConfigEnvelope | null>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<CommandError | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);
  // Latest values for async callbacks. Synced after each commit and written
  // directly whenever a callback replaces them.
  const draftRef = useRef<Config | null>(null);
  const envelopeRef = useRef<ConfigEnvelope | null>(null);
  useLayoutEffect(() => {
    draftRef.current = draft;
    envelopeRef.current = envelope;
  }, [draft, envelope]);

  const notify = useCallback((tone: Toast["tone"], message: string) => {
    toastId.current += 1;
    const id = toastId.current;
    setToasts((current) => [...current.slice(-3), { id, tone, message }]);
    window.setTimeout(() => setToasts((current) => current.filter((t) => t.id !== id)), tone === "error" ? 8000 : 4000);
  }, []);

  const dismissToast = useCallback((id: number) => setToasts((current) => current.filter((t) => t.id !== id)), []);

  const replaceEnvelope = useCallback((next: ConfigEnvelope) => {
    const previous = envelopeRef.current;
    const currentDraft = draftRef.current;
    envelopeRef.current = next;
    setEnvelope(next);
    // Keep unsaved edits; otherwise follow the saved configuration.
    if (!currentDraft || !previous || sameConfig(currentDraft, previous.config)) {
      draftRef.current = structuredClone(next.config);
      setDraft(structuredClone(next.config));
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    (async () => {
      try {
        const [appInfo, config, runtime] = await Promise.all([
          backend.getAppInfo(),
          backend.getConfig(),
          backend.getRuntimeView(),
        ]);
        if (disposed) return;
        setInfo(appInfo);
        setEnvelope(config);
        setDraft(structuredClone(config.config));
        setView(runtime);
        unlisteners.push(await backend.onRuntime((next) => setView(next)));
        unlisteners.push(await backend.onConfig((next) => replaceEnvelope(next)));
      } catch (error) {
        if (!disposed) setLoadError(toCommandError(error).message);
      }
    })();
    return () => {
      disposed = true;
      unlisteners.forEach((u) => u());
    };
  }, [backend, replaceEnvelope]);

  const updateDraft = useCallback((update: (config: Config) => Config) => {
    setDraft((current) => (current ? update(structuredClone(current)) : current));
    setSaveError(null);
  }, []);

  const handleSaveFailure = useCallback(
    async (error: unknown) => {
      const commandError = toCommandError(error);
      setSaveError(commandError);
      if (commandError.code === "conflict") {
        try {
          const latest = await backend.getConfig();
          envelopeRef.current = latest;
          setEnvelope(latest);
          notify("error", "Settings changed elsewhere. Your edits are kept; review and save again.");
        } catch {
          notify("error", commandError.message);
        }
      } else {
        notify("error", commandError.message);
      }
    },
    [backend, notify],
  );

  const saveDraft = useCallback(async () => {
    const current = draftRef.current;
    const saved = envelopeRef.current;
    if (!current || !saved) return false;
    setSaving(true);
    try {
      const next = await backend.saveConfig(current, saved.revision);
      envelopeRef.current = next;
      draftRef.current = structuredClone(next.config);
      setEnvelope(next);
      setDraft(structuredClone(next.config));
      setSaveError(null);
      notify("success", "Settings saved.");
      return true;
    } catch (error) {
      await handleSaveFailure(error);
      return false;
    } finally {
      setSaving(false);
    }
  }, [backend, handleSaveFailure, notify]);

  const discardDraft = useCallback(() => {
    if (envelopeRef.current) setDraft(structuredClone(envelopeRef.current.config));
    setSaveError(null);
  }, []);

  const quickSave = useCallback(
    async (mutate: (config: Config) => Config) => {
      const saved = envelopeRef.current;
      if (!saved) return false;
      const wasDirty = !sameConfig(draftRef.current, saved.config);
      try {
        const next = await backend.saveConfig(mutate(structuredClone(saved.config)), saved.revision);
        envelopeRef.current = next;
        setEnvelope(next);
        setDraft((current) =>
          wasDirty && current ? mutate(structuredClone(current)) : structuredClone(next.config),
        );
        return true;
      } catch (error) {
        await handleSaveFailure(error);
        return false;
      }
    },
    [backend, handleSaveFailure],
  );

  const dirty = useMemo(() => !!envelope && !!draft && !sameConfig(draft, envelope.config), [draft, envelope]);

  const value = useMemo<StoreValue>(
    () => ({
      backend,
      info,
      view,
      envelope,
      draft,
      dirty,
      saving,
      saveError,
      loadError,
      updateDraft,
      saveDraft,
      discardDraft,
      quickSave,
      replaceEnvelope,
      toasts,
      notify,
      dismissToast,
    }),
    [backend, info, view, envelope, draft, dirty, saving, saveError, loadError, updateDraft, saveDraft, discardDraft, quickSave, replaceEnvelope, toasts, notify, dismissToast],
  );

  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

export function useStore(): StoreValue {
  const value = useContext(StoreContext);
  if (!value) throw new Error("useStore must be used inside StoreProvider");
  return value;
}

/** Validation issues from the last failed save that start with `prefix`. */
export function useIssue(prefix: string): string | null {
  const { saveError } = useStore();
  const issue = saveError?.issues.find((i) => i.path === prefix || i.path.startsWith(`${prefix}.`));
  return issue?.message ?? null;
}
