// Mirror of the Rust template parser (crates/deskpresence-core/src/presence_templates.rs)
// used for instant inline validation. The Rust implementation is authoritative.

export const VARIABLES = [
  { name: "app", label: "App name", description: "Display name of the detected application" },
  { name: "exe", label: "Executable", description: "Executable file name, such as zed.exe" },
  { name: "elapsed", label: "Elapsed", description: "Time since the app became the presence (updates each minute)" },
  { name: "window_title", label: "Window title", description: "Title of the app's window (requires the privacy opt-in)" },
] as const;

export type VariableName = (typeof VARIABLES)[number]["name"];

export interface TemplateIssue {
  message: string;
  /** 0-based character offset. */
  position: number;
}

export type Segment = { kind: "text"; value: string } | { kind: "var"; name: VariableName };

const MAX_TEMPLATE_LEN = 512;

function isVariable(name: string): name is VariableName {
  return VARIABLES.some((v) => v.name === name);
}

export function parseTemplate(source: string): { segments: Segment[]; error: TemplateIssue | null } {
  const chars = [...source];
  if (chars.length > MAX_TEMPLATE_LEN) {
    return { segments: [], error: { message: `Template is longer than ${MAX_TEMPLATE_LEN} characters.`, position: MAX_TEMPLATE_LEN } };
  }
  const segments: Segment[] = [];
  let text = "";
  let i = 0;
  while (i < chars.length) {
    const c = chars[i];
    const next = chars[i + 1];
    if (c === "{" && next === "{") {
      text += "{";
      i += 2;
    } else if (c === "}" && next === "}") {
      text += "}";
      i += 2;
    } else if (c === "{") {
      let end = -1;
      for (let j = i + 1; j < chars.length; j++) {
        if (chars[j] === "}") {
          end = j;
          break;
        }
        if (chars[j] === "{") break;
      }
      if (end < 0) {
        return { segments, error: { message: "Unclosed “{”. Use “{{” for a literal brace.", position: i } };
      }
      const raw = chars.slice(i + 1, end).join("").trim();
      if (raw === "") {
        return { segments, error: { message: "Empty variable “{}”.", position: i } };
      }
      const name = raw.toLowerCase();
      if (!isVariable(name)) {
        return {
          segments,
          error: { message: `Unknown variable {${raw}}. Available: {app}, {exe}, {elapsed}, {window_title}.`, position: i },
        };
      }
      if (text) segments.push({ kind: "text", value: text });
      text = "";
      segments.push({ kind: "var", name });
      i = end + 1;
    } else if (c === "}") {
      return { segments, error: { message: "Unexpected “}”. Use “}}” for a literal brace.", position: i } };
    } else {
      text += c;
      i += 1;
    }
  }
  if (text) segments.push({ kind: "text", value: text });
  return { segments, error: null };
}

export function templateError(source: string): TemplateIssue | null {
  return parseTemplate(source).error;
}

export function usesVariable(source: string, name: VariableName): boolean {
  return parseTemplate(source).segments.some((s) => s.kind === "var" && s.name === name);
}

export interface RenderValues {
  app: string;
  exe: string;
  elapsed: string;
  window_title: string;
}

/** Renders like Rust: control characters become spaces and whitespace collapses. */
export function renderTemplate(source: string, values: RenderValues): string | null {
  const { segments, error } = parseTemplate(source);
  if (error) return null;
  const raw = segments.map((s) => (s.kind === "text" ? s.value : values[s.name])).join("");
  // eslint-disable-next-line no-control-regex
  return raw.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
}

/** Length as Discord counts it for the limit (UTF-16 code units). */
export function discordLength(text: string): number {
  return text.length;
}
