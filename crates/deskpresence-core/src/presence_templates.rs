//! Safe presence text templates.
//!
//! Syntax: literal text with `{variable}` placeholders. Use `{{` and `}}` for
//! literal braces. Unknown variables are a validation error, never silently
//! dropped. Rendered text is sanitized (control characters become spaces,
//! whitespace is collapsed) and truncated to Discord's limit on grapheme
//! cluster boundaries so emoji and combining sequences are never split.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::time::format_elapsed;

/// Maximum length of activity text fields (details, state, hover text).
///
/// Discord's current public docs do not state field limits; 128 matches the
/// limits enforced historically by the Discord client and the fixed-size
/// buffers in Discord's official Game SDK. Length is counted in UTF-16 code
/// units, which is never less than the number of code points, so text that
/// fits here also fits a code-point based limit.
pub const DISCORD_TEXT_LIMIT: usize = 128;
/// Discord has historically rejected text fields shorter than two characters.
/// Such fields are omitted (with a warning) instead of being sent.
pub const DISCORD_TEXT_MIN: usize = 2;
/// Maximum template source length accepted by validation.
pub const MAX_TEMPLATE_LEN: usize = 512;

/// Variables that templates may reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum Variable {
    /// Display name of the selected application.
    App,
    /// Executable file name, e.g. `zed.exe`.
    Exe,
    /// Time since the application became the presence source (minute granularity).
    Elapsed,
    /// Title of the application's window. Requires the privacy opt-in.
    WindowTitle,
}

impl Variable {
    pub const ALL: [Variable; 4] = [
        Variable::App,
        Variable::Exe,
        Variable::Elapsed,
        Variable::WindowTitle,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Variable::App => "app",
            Variable::Exe => "exe",
            Variable::Elapsed => "elapsed",
            Variable::WindowTitle => "window_title",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let lowered = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|v| v.name() == lowered)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Var(Variable),
}

/// A parsed, validated template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
}

/// Template problems. Positions are 0-based character offsets.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TemplateError {
    #[error(
        "Unknown variable {{{name}}} at position {position}. Available: {{app}}, {{exe}}, {{elapsed}}, {{window_title}}."
    )]
    UnknownVariable { name: String, position: u32 },
    #[error("Unclosed '{{' at position {position}. Use '{{{{' to write a literal brace.")]
    Unclosed { position: u32 },
    #[error("Unexpected '}}' at position {position}. Use '}}}}' to write a literal brace.")]
    UnmatchedClose { position: u32 },
    #[error("Empty variable '{{}}' at position {position}.")]
    EmptyVariable { position: u32 },
    #[error("Template is longer than {max} characters.")]
    TooLong { max: u32 },
}

fn pos(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

impl Template {
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        if source.chars().count() > MAX_TEMPLATE_LEN {
            return Err(TemplateError::TooLong {
                max: pos(MAX_TEMPLATE_LEN),
            });
        }
        let chars: Vec<char> = source.chars().collect();
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '{' if chars.get(i + 1) == Some(&'{') => {
                    literal.push('{');
                    i += 2;
                }
                '}' if chars.get(i + 1) == Some(&'}') => {
                    literal.push('}');
                    i += 2;
                }
                '{' => {
                    let start = i;
                    let close = chars[i + 1..]
                        .iter()
                        .position(|&c| c == '}' || c == '{')
                        .map(|offset| i + 1 + offset);
                    let Some(end) = close.filter(|&end| chars[end] == '}') else {
                        return Err(TemplateError::Unclosed { position: pos(start) });
                    };
                    let name: String = chars[start + 1..end].iter().collect();
                    if name.trim().is_empty() {
                        return Err(TemplateError::EmptyVariable { position: pos(start) });
                    }
                    let Some(var) = Variable::from_name(&name) else {
                        return Err(TemplateError::UnknownVariable {
                            name: name.trim().to_owned(),
                            position: pos(start),
                        });
                    };
                    if !literal.is_empty() {
                        segments.push(Segment::Literal(std::mem::take(&mut literal)));
                    }
                    segments.push(Segment::Var(var));
                    i = end + 1;
                }
                '}' => {
                    return Err(TemplateError::UnmatchedClose { position: pos(i) });
                }
                c => {
                    literal.push(c);
                    i += 1;
                }
            }
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }
        Ok(Self { segments })
    }

    pub fn uses(&self, variable: Variable) -> bool {
        self.segments
            .iter()
            .any(|s| matches!(s, Segment::Var(v) if *v == variable))
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Renders the template, sanitizing the output. Truncation is a separate
    /// step ([`fit_discord_text`]) so callers can report it.
    pub fn render(&self, ctx: &TemplateContext<'_>) -> Rendered {
        let mut out = String::new();
        let mut window_title_blocked = false;
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Var(Variable::App) => out.push_str(ctx.app),
                Segment::Var(Variable::Exe) => out.push_str(ctx.exe),
                Segment::Var(Variable::Elapsed) => {
                    if let Some(elapsed) = ctx.elapsed {
                        out.push_str(&format_elapsed(elapsed));
                    }
                }
                Segment::Var(Variable::WindowTitle) => {
                    if ctx.window_titles_enabled {
                        out.push_str(ctx.window_title.unwrap_or_default());
                    } else {
                        window_title_blocked = true;
                    }
                }
            }
        }
        Rendered {
            text: sanitize_text(&out),
            window_title_blocked,
        }
    }
}

/// Values available while rendering.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemplateContext<'a> {
    pub app: &'a str,
    pub exe: &'a str,
    pub elapsed: Option<Duration>,
    pub window_title: Option<&'a str>,
    /// Privacy opt-in. When `false`, `{window_title}` renders as empty text.
    pub window_titles_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// `{window_title}` was used while window-title capture is disabled.
    pub window_title_blocked: bool,
}

/// Replaces control characters with spaces, collapses whitespace runs and trims.
pub fn sanitize_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_space = false;
    for c in input.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Result of fitting text into Discord's limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub text: String,
    pub truncated: bool,
    /// Length of the input in UTF-16 code units.
    pub original_len: usize,
}

/// Length as counted for Discord limits (UTF-16 code units).
pub fn discord_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Truncates `text` to at most `limit` UTF-16 code units on a grapheme
/// boundary, appending `…` when anything was removed.
pub fn fit_discord_text(text: &str, limit: usize) -> Fitted {
    let original_len = discord_len(text);
    if original_len <= limit {
        return Fitted {
            text: text.to_owned(),
            truncated: false,
            original_len,
        };
    }
    const ELLIPSIS: char = '…';
    let budget = limit.saturating_sub(ELLIPSIS.len_utf16());
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let len = discord_len(grapheme);
        if used + len > budget {
            break;
        }
        out.push_str(grapheme);
        used += len;
    }
    let mut out = out.trim_end().to_owned();
    if limit >= ELLIPSIS.len_utf16() {
        out.push(ELLIPSIS);
    }
    Fitted {
        text: out,
        truncated: true,
        original_len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TemplateContext<'static> {
        TemplateContext {
            app: "Zed",
            exe: "zed.exe",
            elapsed: Some(Duration::from_secs(125 * 60)),
            window_title: Some("main.rs — project"),
            window_titles_enabled: true,
        }
    }

    #[test]
    fn renders_all_variables() {
        let t = Template::parse("Using {app} ({exe}) for {elapsed}: {window_title}").unwrap();
        assert_eq!(
            t.render(&ctx()).text,
            "Using Zed (zed.exe) for 2h 5m: main.rs — project"
        );
        assert!(t.uses(Variable::Elapsed));
        assert!(!Template::parse("{app}").unwrap().uses(Variable::Elapsed));
    }

    #[test]
    fn variable_names_tolerate_case_and_padding() {
        let t = Template::parse("{ App }").unwrap();
        assert_eq!(t.render(&ctx()).text, "Zed");
    }

    #[test]
    fn escapes_produce_literal_braces() {
        let t = Template::parse("{{app}} = {app} }}").unwrap();
        assert_eq!(t.render(&ctx()).text, "{app} = Zed }");
    }

    #[test]
    fn unknown_variable_is_an_error_with_position() {
        let err = Template::parse("Hi {name}!").unwrap_err();
        assert_eq!(
            err,
            TemplateError::UnknownVariable {
                name: "name".into(),
                position: 3
            }
        );
        assert!(err.to_string().contains("{name}"));
    }

    #[test]
    fn structural_errors() {
        assert_eq!(
            Template::parse("abc {app").unwrap_err(),
            TemplateError::Unclosed { position: 4 }
        );
        assert_eq!(
            Template::parse("{a{pp}").unwrap_err(),
            TemplateError::Unclosed { position: 0 }
        );
        assert_eq!(
            Template::parse("x } y").unwrap_err(),
            TemplateError::UnmatchedClose { position: 2 }
        );
        assert_eq!(
            Template::parse("{ }").unwrap_err(),
            TemplateError::EmptyVariable { position: 0 }
        );
        assert_eq!(
            Template::parse(&"a".repeat(MAX_TEMPLATE_LEN + 1)).unwrap_err(),
            TemplateError::TooLong { max: 512 }
        );
    }

    #[test]
    fn positions_are_character_offsets() {
        let err = Template::parse("héllo {nope}").unwrap_err();
        assert_eq!(
            err,
            TemplateError::UnknownVariable {
                name: "nope".into(),
                position: 6
            }
        );
    }

    #[test]
    fn window_title_requires_opt_in() {
        let t = Template::parse("{app} - {window_title}").unwrap();
        let mut c = ctx();
        c.window_titles_enabled = false;
        let r = t.render(&c);
        assert_eq!(r.text, "Zed -");
        assert!(r.window_title_blocked);
    }

    #[test]
    fn missing_elapsed_renders_empty() {
        let t = Template::parse("{app} {elapsed}").unwrap();
        let mut c = ctx();
        c.elapsed = None;
        assert_eq!(t.render(&c).text, "Zed");
    }

    #[test]
    fn sanitizes_control_characters_and_whitespace() {
        assert_eq!(sanitize_text("  a\n\tb\u{0}  c  "), "a b c");
        assert_eq!(sanitize_text("\n\n"), "");
    }

    #[test]
    fn truncation_keeps_short_text() {
        let f = fit_discord_text("hello", 128);
        assert_eq!(f.text, "hello");
        assert!(!f.truncated);
    }

    #[test]
    fn truncation_is_exact_at_limit() {
        let exact = "a".repeat(128);
        assert!(!fit_discord_text(&exact, 128).truncated);
        let over = "a".repeat(129);
        let f = fit_discord_text(&over, 128);
        assert!(f.truncated);
        assert_eq!(discord_len(&f.text), 128);
        assert!(f.text.ends_with('…'));
        assert_eq!(f.original_len, 129);
    }

    #[test]
    fn truncation_never_splits_graphemes() {
        // Family emoji: one grapheme, 11 UTF-16 units.
        let family = "👨‍👩‍👧‍👦";
        assert_eq!(discord_len(family), 11);
        let text = format!("{}{}", "x".repeat(120), family);
        let f = fit_discord_text(&text, 128);
        assert!(f.truncated);
        assert_eq!(f.text, format!("{}…", "x".repeat(120)));

        // Combining accent stays attached to its base letter.
        let combining = "e\u{301}".repeat(100);
        let f = fit_discord_text(&combining, 9);
        assert!(f.text.ends_with('…'));
        let body = f.text.trim_end_matches('…');
        assert_eq!(body, "e\u{301}".repeat(4));
    }

    #[test]
    fn truncation_counts_astral_characters_as_two_units() {
        let text = "😀".repeat(70); // 140 UTF-16 units
        let f = fit_discord_text(&text, 128);
        assert!(f.truncated);
        assert!(discord_len(&f.text) <= 128);
        assert!(f.text.chars().filter(|&c| c == '😀').count() == 63);
    }

    #[test]
    fn truncation_trims_trailing_space_before_ellipsis() {
        let text = format!("{} {}", "a".repeat(126), "b".repeat(10));
        let f = fit_discord_text(&text, 128);
        assert_eq!(f.text, format!("{}…", "a".repeat(126)));
    }
}
