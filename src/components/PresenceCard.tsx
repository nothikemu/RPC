import { Clock, ImageOff } from "lucide-react";
import { formatDuration } from "../lib/format";
import { activityTypeLabel } from "../lib/labels";
import type { Activity, ActivityType } from "../lib/types";
import { useNow } from "./useNow";

const typeByCode: Record<number, ActivityType> = { 0: "playing", 2: "listening", 3: "watching", 5: "competing" };

/**
 * Approximates how Discord lays out Rich Presence. Artwork is shown as its
 * asset key: DeskPresence never downloads Discord assets or uploads local icons.
 */
export function PresenceCard({
  activity,
  emptyText,
  compact = false,
}: {
  activity: Activity | null;
  emptyText: string;
  compact?: boolean;
}) {
  const start = activity?.timestamps?.start ?? null;
  const now = useNow(1000, start != null);
  if (!activity) {
    return (
      <div className={`presence-card is-empty${compact ? " is-compact" : ""}`}>
        <p className="presence-empty">{emptyText}</p>
      </div>
    );
  }
  const type = typeByCode[activity.type] ?? "playing";
  const large = activity.assets?.large_image;
  const small = activity.assets?.small_image;
  return (
    <div className={`presence-card${compact ? " is-compact" : ""}`} aria-label="Rich Presence preview">
      <p className="presence-heading">
        {activityTypeLabel[type]} {activity.name ? <strong>{activity.name}</strong> : <span className="presence-appname">your Discord application</span>}
      </p>
      <div className="presence-body">
        <div className="presence-art" title={large ? `Large image: ${large}` : "No large image set: Discord shows your application's icon"}>
          {large ? <span className="presence-art-key">{large}</span> : <ImageOff size={20} aria-hidden />}
          {small ? (
            <span className="presence-art-small" title={`Small image: ${small}`}>
              {small.slice(0, 2)}
            </span>
          ) : null}
        </div>
        <div className="presence-lines">
          {activity.details ? <p className="presence-details">{activity.details}</p> : null}
          {activity.state ? <p className="presence-state">{activity.state}</p> : null}
          {start != null ? (
            <p className="presence-time">
              <Clock size={12} aria-hidden />
              {formatDuration(now - start * 1000)} elapsed
            </p>
          ) : null}
          {!activity.details && !activity.state && start == null ? <p className="presence-state">No text fields</p> : null}
        </div>
      </div>
    </div>
  );
}
