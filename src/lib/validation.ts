// Client-side validation mirrors the Rust rules for instant feedback.
// Rust re-validates every change; these checks never replace it.

export const POLL_INTERVAL = { min: 250, max: 10_000, default: 1_000 } as const;
export const DEBOUNCE = { min: 0, max: 5_000, default: 500 } as const;
export const PRIORITY = { min: -1000, max: 1000 } as const;
export const DISCORD_TEXT_LIMIT = 128;
export const MAX_ASSET_LEN = 256;

/**
 * Discord application IDs are snowflakes: 17–20 decimal digits.
 * A syntactically valid ID may still not exist or be rejected by Discord.
 */
export function isValidApplicationId(value: string): boolean {
  if (!/^\d{17,20}$/.test(value)) return false;
  try {
    const n = BigInt(value);
    return n > 0n && n <= 18_446_744_073_709_551_615n;
  } catch {
    return false;
  }
}

export function applicationIdError(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed === "") return null;
  if (!/^\d+$/.test(trimmed)) return "Use digits only. Copy the Application ID from the Developer Portal.";
  if (trimmed.length < 17 || trimmed.length > 20) return "Application IDs are 17 to 20 digits long.";
  if (!isValidApplicationId(trimmed)) return "This number is out of range for an Application ID.";
  return null;
}

/** Asset values are Discord asset keys or https:// image URLs. */
export function assetValueError(value: string): string | null {
  const v = value.trim();
  if (v === "") return null;
  if ([...v].length > MAX_ASSET_LEN) return `Use at most ${MAX_ASSET_LEN} characters.`;
  if (/\s/.test(v)) return "Asset keys can't contain spaces.";
  const lower = v.toLowerCase();
  if (lower.startsWith("http://")) return "Image URLs must start with https://.";
  if (lower.includes("://") && !lower.startsWith("https://")) return "Use an asset key or an https:// image URL.";
  return null;
}

export type MatcherKind = "path" | "exe_name" | "pattern";

export function matcherError(kind: MatcherKind, value: string): string | null {
  const v = value.trim();
  if (v === "") return "Enter a value.";
  if ([...v].length > 1024) return "Use at most 1024 characters.";
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(v)) return "Remove control characters.";
  if (kind === "exe_name" && /[\\/]/.test(v)) return "An executable name can't contain \\ or /. Use a path instead.";
  if (kind === "pattern" && /^[*?]+$/.test(v)) return "This pattern would match everything.";
  if (kind === "path" && !/^([a-zA-Z]:[\\/]|\\\\)/.test(v)) return "Enter a full path, such as C:\\Program Files\\App\\app.exe.";
  return null;
}

export function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) return min;
  return Math.min(max, Math.max(min, value));
}
