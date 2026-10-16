// Formatting helpers for times and durations shown in the UI.

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

export function formatRelative(ms: number | null | undefined, now: number = Date.now()): string {
  if (ms == null) return "Never";
  const diff = now - ms;
  const future = diff < 0;
  const abs = Math.abs(diff);
  const sec = Math.round(abs / 1000);
  let text: string;
  if (sec < 5) return future ? "in a moment" : "just now";
  if (sec < 60) text = `${sec} s`;
  else if (sec < 3600) text = `${Math.round(sec / 60)} min`;
  else if (sec < 86_400) text = `${Math.round(sec / 3600)} h`;
  else text = `${Math.round(sec / 86_400)} d`;
  return future ? `in ${text}` : `${text} ago`;
}

export function formatTime(ms: number | null | undefined): string {
  if (ms == null) return "—";
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

export function formatDateTime(ms: number | null | undefined): string {
  if (ms == null) return "—";
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatMicros(us: number): string {
  if (us < 1000) return `${us} µs`;
  return `${(us / 1000).toFixed(us < 10_000 ? 1 : 0)} ms`;
}

export function newRuleId(prefix: "r" | "x"): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return `${prefix}-${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}
