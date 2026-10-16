//! Canonical application identity and display-name resolution.
//!
//! An application is identified by its *normalized executable path*
//! ([`AppKey`]). Display names are presentation only: renaming an
//! application never changes its identity, so rules keep matching.

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

/// Stable identity of a detected application: the normalized, lower-cased
/// executable path (for example `c:\program files\app\app.exe`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AppKey(String);

impl AppKey {
    /// Builds a key from a raw executable path. Returns `None` for empty paths.
    pub fn from_path(path: &str) -> Option<Self> {
        let normalized = normalize_path(path);
        if normalized.is_empty() {
            None
        } else {
            Some(Self(normalized))
        }
    }

    /// Wraps an already-normalized key (for example one received from the UI).
    /// The value is re-normalized so callers cannot smuggle in a different form.
    pub fn parse(value: &str) -> Option<Self> {
        Self::from_path(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lower-cased executable file name derived from the key.
    pub fn exe_name(&self) -> &str {
        self.0.rsplit('\\').next().unwrap_or(&self.0)
    }
}

impl std::fmt::Display for AppKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalizes a Windows path for identity comparisons:
/// trims whitespace and quotes, removes the `\\?\` prefix, converts `/` to `\`,
/// collapses duplicate separators, resolves `.` and `..`, drops trailing
/// separators and lower-cases the result.
///
/// The function is deliberately purely lexical: it never touches the file
/// system, so it is safe to call for paths that no longer exist.
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let unified = trimmed.replace('/', "\\");

    // Strip verbatim prefixes.
    let (unc, rest) = if let Some(rest) = unified.strip_prefix("\\\\?\\UNC\\") {
        (true, rest.to_owned())
    } else if let Some(rest) = unified.strip_prefix("\\\\?\\") {
        (false, rest.to_owned())
    } else if let Some(rest) = unified.strip_prefix("\\\\.\\") {
        (false, rest.to_owned())
    } else if let Some(rest) = unified.strip_prefix("\\\\") {
        (true, rest.to_owned())
    } else {
        (false, unified)
    };

    let mut parts: Vec<&str> = Vec::new();
    for component in rest.split('\\') {
        match component {
            "" | "." => {}
            ".." => {
                // Never pop the drive (`c:`) or the UNC `server\share` root.
                let root_len = if unc {
                    2
                } else if parts.first().is_some_and(|p| is_drive(p)) {
                    1
                } else {
                    0
                };
                if parts.len() > root_len {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }

    let joined = parts.join("\\");
    let joined = if unc { format!("\\\\{joined}") } else { joined };
    joined.to_lowercase()
}

fn is_drive(component: &str) -> bool {
    component.len() == 2 && component.ends_with(':')
}

/// Returns the file-name component of a path in its original casing.
pub fn exe_file_name(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed).to_owned()
}

/// Lower-cases and trims an executable name for comparisons.
pub fn normalize_exe_name(name: &str) -> String {
    name.trim().trim_matches('"').trim().to_lowercase()
}

/// Returns the executable name without a trailing `.exe` (case-insensitive).
pub fn exe_stem(name: &str) -> &str {
    let len = name.len();
    if len > 4 && name.is_char_boundary(len - 4) && name[len - 4..].eq_ignore_ascii_case(".exe") {
        &name[..len - 4]
    } else {
        name
    }
}

/// Version-resource metadata read from an executable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExeMetadata {
    pub file_description: Option<String>,
    pub product_name: Option<String>,
}

/// Where a display name came from, shown in the UI so users understand it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum NameSource {
    /// Configured by the user in an application rule.
    User,
    /// Read from the executable's version resource.
    Metadata,
    /// From DeskPresence's small built-in table of generic hosts.
    KnownMapping,
    /// Derived from the executable file name.
    ExecutableName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedName {
    pub name: String,
    pub source: NameSource,
}

/// Maximum length (in grapheme clusters) accepted for a metadata-derived name.
const MAX_METADATA_NAME: usize = 64;

/// Generic strings that appear in version resources but say nothing about the
/// application itself. Compared case-insensitively after cleanup (trademark
/// markers removed, whitespace collapsed).
const GENERIC_METADATA_NAMES: &[&str] = &[
    "application",
    "app",
    "electron",
    "java platform se binary",
    "openjdk platform binary",
    "microsoft windows operating system",
    "windows operating system",
    "operating system",
    "host process",
    "launcher",
    "unknown",
    "n/a",
    "none",
    "default",
];

/// Local fallback names for generic runtime hosts whose version metadata does
/// not describe the program being run. Keyed by lower-cased executable name.
/// This table is intentionally tiny and never consulted online.
const KNOWN_NAMES: &[(&str, &str)] = &[
    ("java.exe", "Java"),
    ("javaw.exe", "Java"),
    ("python.exe", "Python"),
    ("pythonw.exe", "Python"),
    ("node.exe", "Node.js"),
    ("electron.exe", "Electron app"),
    ("cmd.exe", "Command Prompt"),
    ("powershell.exe", "Windows PowerShell"),
    ("pwsh.exe", "PowerShell"),
];

/// Resolves the display name in the documented order:
/// 1. user-configured name, 2. reliable executable metadata,
/// 3. the local known-name table, 4. the executable name without `.exe`.
pub fn resolve_display_name(
    user_name: Option<&str>,
    metadata: Option<&ExeMetadata>,
    exe_name: &str,
) -> ResolvedName {
    if let Some(name) = user_name.map(str::trim).filter(|n| !n.is_empty()) {
        return ResolvedName {
            name: name.to_owned(),
            source: NameSource::User,
        };
    }

    if let Some(meta) = metadata {
        for candidate in [&meta.file_description, &meta.product_name].into_iter().flatten() {
            if let Some(clean) = clean_metadata_name(candidate) {
                return ResolvedName {
                    name: clean,
                    source: NameSource::Metadata,
                };
            }
        }
    }

    let lowered = normalize_exe_name(exe_name);
    if let Some((_, name)) = KNOWN_NAMES.iter().find(|(exe, _)| *exe == lowered) {
        return ResolvedName {
            name: (*name).to_owned(),
            source: NameSource::KnownMapping,
        };
    }

    ResolvedName {
        name: prettify_stem(exe_stem(exe_name.trim())),
        source: NameSource::ExecutableName,
    }
}

/// Cleans a version-resource string and decides whether it is trustworthy.
fn clean_metadata_name(raw: &str) -> Option<String> {
    let mut cleaned = raw.replace(['®', '™', '©', '\u{0}'], "");
    for marker in ["(R)", "(r)", "(TM)", "(tm)", "(C)"] {
        cleaned = cleaned.replace(marker, "");
    }
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    if cleaned.is_empty() || cleaned.graphemes(true).count() > MAX_METADATA_NAME {
        return None;
    }
    if !cleaned.chars().any(char::is_alphabetic) {
        return None;
    }
    let lower = cleaned.to_lowercase();
    if GENERIC_METADATA_NAMES.contains(&lower.as_str()) {
        return None;
    }
    if lower.ends_with(".exe") || lower.ends_with(".dll") {
        return None;
    }
    Some(cleaned)
}

/// Turns `zed` into `Zed`; leaves mixed-case names such as `RobloxPlayerBeta` alone.
fn prettify_stem(stem: &str) -> String {
    if stem.is_empty() {
        return "Unknown application".to_owned();
    }
    let all_lower = stem.chars().all(|c| !c.is_alphabetic() || c.is_lowercase());
    if all_lower {
        let mut chars = stem.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_separators_and_prefixes() {
        assert_eq!(
            normalize_path(r"C:\Program Files\Foo\Foo.EXE"),
            r"c:\program files\foo\foo.exe"
        );
        assert_eq!(
            normalize_path(r"\\?\C:\Program Files\Foo\foo.exe"),
            r"c:\program files\foo\foo.exe"
        );
        assert_eq!(
            normalize_path("C:/Program Files//Foo/./bar/../foo.exe"),
            r"c:\program files\foo\foo.exe"
        );
        assert_eq!(normalize_path("  \"C:\\Tools\\x.exe\"  "), r"c:\tools\x.exe");
        assert_eq!(normalize_path(r"C:\Tools\"), r"c:\tools");
    }

    #[test]
    fn normalizes_unc_paths_without_escaping_the_share() {
        assert_eq!(
            normalize_path(r"\\Server\Share\Apps\x.exe"),
            r"\\server\share\apps\x.exe"
        );
        assert_eq!(
            normalize_path(r"\\?\UNC\server\share\x.exe"),
            r"\\server\share\x.exe"
        );
        assert_eq!(normalize_path(r"C:\..\..\x.exe"), r"c:\x.exe");
    }

    #[test]
    fn empty_paths_have_no_key() {
        assert!(AppKey::from_path("   ").is_none());
        assert!(AppKey::from_path("\"\"").is_none());
    }

    #[test]
    fn key_is_stable_across_spellings() {
        let a = AppKey::from_path(r"C:\Apps\Zed\Zed.exe");
        let b = AppKey::from_path(r"\\?\c:/apps/zed/zed.exe");
        assert_eq!(a, b);
        assert_eq!(a.map(|k| k.exe_name().to_owned()), Some("zed.exe".into()));
    }

    #[test]
    fn exe_helpers() {
        assert_eq!(exe_file_name(r"C:\Apps\Zed\Zed.exe"), "Zed.exe");
        assert_eq!(exe_file_name("Zed.exe"), "Zed.exe");
        assert_eq!(exe_stem("Zed.EXE"), "Zed");
        assert_eq!(exe_stem("tool"), "tool");
        assert_eq!(exe_stem(".exe"), ".exe");
        assert_eq!(normalize_exe_name(" Zed.EXE "), "zed.exe");
    }

    fn meta(desc: Option<&str>, product: Option<&str>) -> ExeMetadata {
        ExeMetadata {
            file_description: desc.map(str::to_owned),
            product_name: product.map(str::to_owned),
        }
    }

    #[test]
    fn user_name_wins() {
        let r = resolve_display_name(Some(" Editor "), Some(&meta(Some("Desc"), None)), "x.exe");
        assert_eq!(r.name, "Editor");
        assert_eq!(r.source, NameSource::User);
    }

    #[test]
    fn metadata_prefers_description_and_strips_marks() {
        let r = resolve_display_name(
            None,
            Some(&meta(Some("Contoso® Paint™"), Some("Contoso Suite"))),
            "paint.exe",
        );
        assert_eq!(r.name, "Contoso Paint");
        assert_eq!(r.source, NameSource::Metadata);
    }

    #[test]
    fn generic_metadata_falls_through_to_product_then_fallbacks() {
        let r = resolve_display_name(
            None,
            Some(&meta(Some("Electron"), Some("Fancy Chat"))),
            "fancy.exe",
        );
        assert_eq!(r.name, "Fancy Chat");

        let r = resolve_display_name(
            None,
            Some(&meta(
                Some("Java(TM) Platform SE binary"),
                Some("Java(TM) Platform SE 8"),
            )),
            "javaw.exe",
        );
        // Product name is not generic after cleanup, so it is still used.
        assert_eq!(r.name, "Java Platform SE 8");

        let r = resolve_display_name(None, Some(&meta(Some("  "), Some("123"))), "javaw.exe");
        assert_eq!(r.source, NameSource::KnownMapping);
        assert_eq!(r.name, "Java");
    }

    #[test]
    fn executable_name_fallback() {
        let r = resolve_display_name(None, None, "zed.exe");
        assert_eq!(r.name, "Zed");
        assert_eq!(r.source, NameSource::ExecutableName);
        let r = resolve_display_name(None, None, "MyGameClient.exe");
        assert_eq!(r.name, "MyGameClient");
        let r = resolve_display_name(None, Some(&meta(Some("tool.exe"), None)), "tool.exe");
        assert_eq!(r.name, "Tool");
    }

    #[test]
    fn overly_long_metadata_is_rejected() {
        let long = "word ".repeat(30);
        let r = resolve_display_name(None, Some(&meta(Some(&long), None)), "x.exe");
        assert_eq!(r.source, NameSource::ExecutableName);
    }
}
