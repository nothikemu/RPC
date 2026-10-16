import { useStore } from "../state/store";
import { Button } from "./ui";

/** Sticky bar shown whenever settings have unsaved changes. */
export function SaveBar() {
  const { dirty, saving, saveDraft, discardDraft, saveError } = useStore();
  if (!dirty) return null;
  return (
    <div className="save-bar" role="region" aria-label="Unsaved changes">
      <div className="save-bar-text">
        <strong>Unsaved changes</strong>
        {saveError ? <span className="save-bar-error">{saveError.message}</span> : <span>Changes apply when you save.</span>}
      </div>
      <div className="save-bar-actions">
        <Button variant="ghost" onClick={discardDraft} disabled={saving}>
          Discard
        </Button>
        <Button variant="primary" onClick={() => void saveDraft()} loading={saving}>
          Save changes
        </Button>
      </div>
    </div>
  );
}
