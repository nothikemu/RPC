//! Typed application rules, matching, exclusions and precedence.
//!
//! # Precedence
//!
//! 1. **Exclusions win.** If any exclusion rule matches an application, it can
//!    never control the presence — not through foreground, priority or pinning.
//! 2. **One application rule applies.** The applicable rule is chosen by match
//!    specificity: an exact normalized *path* beats an exact *executable name*,
//!    which beats a *pattern*. Among patterns, the first in list order wins.
//! 3. An applicable rule with `enabled = false` makes the application
//!    ineligible, but it is still listed in the UI and tray-independent views.
//!
//! Matching never uses display names, so renaming an application cannot
//! break its rules.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::app_identity::{AppKey, normalize_exe_name, normalize_path};

/// Maximum length of a matcher value (paths can be long on Windows).
pub const MAX_MATCHER_LEN: usize = 1024;
/// Allowed priority range. Higher numbers win in priority mode.
pub const PRIORITY_MIN: i32 = -1000;
pub const PRIORITY_MAX: i32 = 1000;

/// How a matcher compares against an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// Exact normalized executable path, e.g. `C:\Program Files\App\app.exe`.
    Path,
    /// Exact executable file name, e.g. `app.exe`.
    ExeName,
    /// Case-insensitive wildcard pattern with `*` and `?`. Patterns that
    /// contain `\` or `/` match the full path; others match the file name.
    Pattern,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AppMatcher {
    pub kind: MatchKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MatcherError {
    #[error("the value is empty")]
    Empty,
    #[error("the value is longer than {MAX_MATCHER_LEN} characters")]
    TooLong,
    #[error("the value contains control characters")]
    ControlCharacters,
    #[error("an executable name cannot contain path separators; use a path rule instead")]
    SeparatorInExeName,
    #[error("a pattern must not consist only of wildcards")]
    PatternTooBroad,
}

impl AppMatcher {
    pub fn path(value: impl Into<String>) -> Self {
        Self {
            kind: MatchKind::Path,
            value: value.into(),
        }
    }

    pub fn exe_name(value: impl Into<String>) -> Self {
        Self {
            kind: MatchKind::ExeName,
            value: value.into(),
        }
    }

    pub fn pattern(value: impl Into<String>) -> Self {
        Self {
            kind: MatchKind::Pattern,
            value: value.into(),
        }
    }

    /// Validates the matcher. Validation is intentionally permissive about
    /// *existence*: a path rule for an application that is not installed is
    /// legitimate (the UI shows a warning instead).
    pub fn validate(&self) -> Result<(), MatcherError> {
        let value = self.value.trim();
        if value.is_empty() {
            return Err(MatcherError::Empty);
        }
        if value.chars().count() > MAX_MATCHER_LEN {
            return Err(MatcherError::TooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(MatcherError::ControlCharacters);
        }
        match self.kind {
            MatchKind::ExeName if value.contains(['\\', '/']) => Err(MatcherError::SeparatorInExeName),
            MatchKind::Pattern if value.chars().all(|c| matches!(c, '*' | '?')) => {
                Err(MatcherError::PatternTooBroad)
            }
            _ => Ok(()),
        }
    }

    /// The comparison form of the value.
    fn normalized_value(&self) -> String {
        match self.kind {
            MatchKind::Path => normalize_path(&self.value),
            MatchKind::ExeName => normalize_exe_name(&self.value),
            MatchKind::Pattern => self.value.trim().replace('/', "\\").to_lowercase(),
        }
    }

    pub fn matches(&self, facts: &AppFacts<'_>) -> bool {
        let value = self.normalized_value();
        match self.kind {
            MatchKind::Path => value == facts.key.as_str(),
            MatchKind::ExeName => value == facts.exe_name(),
            MatchKind::Pattern => {
                if value.contains('\\') {
                    glob_match(&value, facts.key.as_str())
                } else {
                    glob_match(&value, facts.exe_name())
                }
            }
        }
    }

    /// Returns `true` when both matchers describe exactly the same set.
    pub fn same_target(&self, other: &AppMatcher) -> bool {
        self.kind == other.kind && self.normalized_value() == other.normalized_value()
    }
}

/// The facts about an application that rules may match on.
#[derive(Clone, Copy, Debug)]
pub struct AppFacts<'a> {
    pub key: &'a AppKey,
}

impl<'a> AppFacts<'a> {
    pub fn new(key: &'a AppKey) -> Self {
        Self { key }
    }

    fn exe_name(&self) -> &str {
        self.key.exe_name()
    }
}

/// Optional Discord artwork configured for an application (or globally).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[serde(default)]
pub struct AssetOverrides {
    /// Asset key uploaded to the Discord application, or an https:// image URL.
    pub large_image: Option<String>,
    pub large_text: Option<String>,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
}

impl AssetOverrides {
    pub fn is_empty(&self) -> bool {
        self.large_image.is_none()
            && self.large_text.is_none()
            && self.small_image.is_none()
            && self.small_text.is_none()
    }
}

/// Per-application rule. Identified by a stable `id`, never by display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AppRule {
    pub id: String,
    pub matcher: AppMatcher,
    /// Optional user-facing name; overrides executable metadata.
    #[serde(default)]
    pub display_name: Option<String>,
    /// When `false`, the application never controls the presence automatically.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Higher wins in priority mode. Default 0.
    #[serde(default)]
    pub priority: i32,
    /// Overrides the global Details template when set and non-empty.
    #[serde(default)]
    pub details_template: Option<String>,
    /// Overrides the global State template when set and non-empty.
    #[serde(default)]
    pub state_template: Option<String>,
    #[serde(default)]
    pub assets: AssetOverrides,
}

fn default_true() -> bool {
    true
}

impl AppRule {
    pub fn new(id: impl Into<String>, matcher: AppMatcher) -> Self {
        Self {
            id: id.into(),
            matcher,
            display_name: None,
            enabled: true,
            priority: 0,
            details_template: None,
            state_template: None,
            assets: AssetOverrides::default(),
        }
    }
}

/// Excludes matching applications from the presence and the tray menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ExclusionRule {
    pub id: String,
    pub matcher: AppMatcher,
    #[serde(default)]
    pub note: Option<String>,
}

/// Generates a short random identifier for rules, e.g. `r-5f1c0a9e3b2d4c7a`.
pub fn new_rule_id(prefix: &str) -> String {
    format!("{prefix}-{:016x}", fastrand::u64(..))
}

/// The result of evaluating rules for one application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eligibility {
    Eligible,
    /// Excluded by the exclusion rule at this index.
    Excluded(usize),
    /// Disabled by the application rule at this index.
    Disabled(usize),
}

impl Eligibility {
    pub fn is_eligible(self) -> bool {
        matches!(self, Eligibility::Eligible)
    }
}

/// Rules preprocessed for fast lookup during each detection pass.
#[derive(Debug, Default, Clone)]
pub struct CompiledRules {
    by_path: HashMap<String, usize>,
    by_exe: HashMap<String, usize>,
    patterns: Vec<(usize, String)>,
    /// `(index into the exclusion list, matcher)`; invalid entries are skipped
    /// but indices still refer to the original list.
    exclusions: Vec<(usize, AppMatcher)>,
}

/// Which rule (by index into the rule list) applies, and how it matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleMatch {
    pub index: usize,
    pub kind: MatchKind,
}

impl CompiledRules {
    pub fn new(rules: &[AppRule], exclusions: &[ExclusionRule]) -> Self {
        let mut compiled = CompiledRules::default();
        for (index, rule) in rules.iter().enumerate() {
            if rule.matcher.validate().is_err() {
                continue;
            }
            let value = rule.matcher.normalized_value();
            match rule.matcher.kind {
                // `entry().or_insert` keeps the first rule for duplicates.
                MatchKind::Path => {
                    compiled.by_path.entry(value).or_insert(index);
                }
                MatchKind::ExeName => {
                    compiled.by_exe.entry(value).or_insert(index);
                }
                MatchKind::Pattern => compiled.patterns.push((index, value)),
            }
        }
        compiled.exclusions = exclusions
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matcher.validate().is_ok())
            .map(|(index, e)| (index, e.matcher.clone()))
            .collect();
        compiled
    }

    /// Finds the single applicable rule using the documented precedence.
    pub fn find_rule(&self, facts: &AppFacts<'_>) -> Option<RuleMatch> {
        if let Some(&index) = self.by_path.get(facts.key.as_str()) {
            return Some(RuleMatch {
                index,
                kind: MatchKind::Path,
            });
        }
        if let Some(&index) = self.by_exe.get(facts.exe_name()) {
            return Some(RuleMatch {
                index,
                kind: MatchKind::ExeName,
            });
        }
        self.patterns.iter().find_map(|(index, pattern)| {
            let subject = if pattern.contains('\\') {
                facts.key.as_str()
            } else {
                facts.exe_name()
            };
            glob_match(pattern, subject).then_some(RuleMatch {
                index: *index,
                kind: MatchKind::Pattern,
            })
        })
    }

    /// Index of the first matching exclusion rule.
    pub fn find_exclusion(&self, facts: &AppFacts<'_>) -> Option<usize> {
        self.exclusions
            .iter()
            .find(|(_, m)| m.matches(facts))
            .map(|(index, _)| *index)
    }

    pub fn eligibility(&self, facts: &AppFacts<'_>, rules: &[AppRule]) -> Eligibility {
        if let Some(index) = self.find_exclusion(facts) {
            return Eligibility::Excluded(index);
        }
        match self.find_rule(facts) {
            Some(m) if rules.get(m.index).is_some_and(|r| !r.enabled) => Eligibility::Disabled(m.index),
            _ => Eligibility::Eligible,
        }
    }
}

/// Case-sensitive glob match (callers lower-case both sides) supporting `*`
/// (any run of characters, including separators) and `?` (one character).
/// Iterative with single-star backtracking: O(n·m) worst case, no recursion.
pub fn glob_match(pattern: &str, subject: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = subject.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_si = 0usize;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_si = si;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_si += 1;
            si = star_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> AppKey {
        AppKey::from_path(path).unwrap_or_else(|| panic!("bad test path {path}"))
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("*.exe", "zed.exe"));
        assert!(glob_match("z?d.exe", "zed.exe"));
        assert!(!glob_match("z?d.exe", "zeed.exe"));
        assert!(glob_match("c:\\games\\*", "c:\\games\\a\\b.exe"));
        assert!(glob_match("*steam*", "steamwebhelper.exe"));
        assert!(!glob_match("abc", "abcd"));
        assert!(glob_match("a*b*c", "a__b__c"));
        assert!(!glob_match("a*b*c", "a__c__b"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn matcher_kinds() {
        let k = key(r"C:\Program Files\Zed\Zed.exe");
        let facts = AppFacts::new(&k);
        assert!(AppMatcher::path(r"c:/program files/zed/zed.EXE").matches(&facts));
        assert!(AppMatcher::exe_name("ZED.exe").matches(&facts));
        assert!(AppMatcher::pattern("z*.exe").matches(&facts));
        assert!(AppMatcher::pattern(r"C:\Program Files\*").matches(&facts));
        assert!(AppMatcher::pattern("C:/Program Files/*").matches(&facts));
        assert!(!AppMatcher::exe_name("zed").matches(&facts));
        assert!(!AppMatcher::path(r"D:\Zed\zed.exe").matches(&facts));
    }

    #[test]
    fn matcher_validation() {
        assert_eq!(AppMatcher::exe_name("  ").validate(), Err(MatcherError::Empty));
        assert_eq!(
            AppMatcher::exe_name(r"C:\x.exe").validate(),
            Err(MatcherError::SeparatorInExeName)
        );
        assert_eq!(
            AppMatcher::pattern("**").validate(),
            Err(MatcherError::PatternTooBroad)
        );
        assert_eq!(
            AppMatcher::path("C:\\a\u{7}.exe").validate(),
            Err(MatcherError::ControlCharacters)
        );
        assert_eq!(
            AppMatcher::path("x".repeat(MAX_MATCHER_LEN + 1)).validate(),
            Err(MatcherError::TooLong)
        );
        // Non-existent paths are legitimate custom configuration.
        assert!(AppMatcher::path(r"Z:\Not\Installed.exe").validate().is_ok());
    }

    #[test]
    fn path_beats_exe_beats_pattern_and_first_pattern_wins() {
        let rules = vec![
            AppRule::new("p1", AppMatcher::pattern("*.exe")),
            AppRule::new("p2", AppMatcher::pattern("zed*")),
            AppRule::new("e", AppMatcher::exe_name("zed.exe")),
            AppRule::new("p", AppMatcher::path(r"C:\Zed\zed.exe")),
        ];
        let compiled = CompiledRules::new(&rules, &[]);

        let exact = key(r"C:\Zed\zed.exe");
        let m = compiled.find_rule(&AppFacts::new(&exact));
        assert_eq!(
            m,
            Some(RuleMatch {
                index: 3,
                kind: MatchKind::Path
            })
        );

        let other_location = key(r"D:\Portable\zed.exe");
        let m = compiled.find_rule(&AppFacts::new(&other_location));
        assert_eq!(
            m,
            Some(RuleMatch {
                index: 2,
                kind: MatchKind::ExeName
            })
        );

        let only_pattern = key(r"D:\zeditor.exe");
        let m = compiled.find_rule(&AppFacts::new(&only_pattern));
        assert_eq!(
            m,
            Some(RuleMatch {
                index: 0,
                kind: MatchKind::Pattern
            })
        );
    }

    #[test]
    fn duplicate_exact_rules_keep_the_first() {
        let rules = vec![
            AppRule::new("first", AppMatcher::exe_name("a.exe")),
            AppRule::new("second", AppMatcher::exe_name("A.EXE")),
        ];
        let compiled = CompiledRules::new(&rules, &[]);
        let k = key(r"C:\a.exe");
        assert_eq!(compiled.find_rule(&AppFacts::new(&k)).map(|m| m.index), Some(0));
    }

    #[test]
    fn invalid_rules_are_ignored_rather_than_crashing() {
        let rules = vec![AppRule::new("bad", AppMatcher::exe_name(""))];
        let compiled = CompiledRules::new(&rules, &[]);
        let k = key(r"C:\a.exe");
        assert_eq!(compiled.find_rule(&AppFacts::new(&k)), None);
    }

    #[test]
    fn exclusion_takes_precedence_over_enabled_rules() {
        let mut rule = AppRule::new("r", AppMatcher::path(r"C:\Games\game.exe"));
        rule.priority = 100;
        let rules = vec![rule];
        let exclusions = vec![ExclusionRule {
            id: "x".into(),
            matcher: AppMatcher::pattern(r"c:\games\*"),
            note: None,
        }];
        let compiled = CompiledRules::new(&rules, &exclusions);
        let k = key(r"C:\Games\game.exe");
        assert_eq!(
            compiled.eligibility(&AppFacts::new(&k), &rules),
            Eligibility::Excluded(0)
        );
    }

    #[test]
    fn exclusion_indices_survive_invalid_entries() {
        let exclusions = vec![
            ExclusionRule {
                id: "bad".into(),
                matcher: AppMatcher::exe_name(""),
                note: None,
            },
            ExclusionRule {
                id: "good".into(),
                matcher: AppMatcher::exe_name("a.exe"),
                note: None,
            },
        ];
        let compiled = CompiledRules::new(&[], &exclusions);
        let k = key(r"C:\a.exe");
        assert_eq!(compiled.find_exclusion(&AppFacts::new(&k)), Some(1));
    }

    #[test]
    fn disabled_rule_makes_app_ineligible() {
        let mut rule = AppRule::new("r", AppMatcher::exe_name("chat.exe"));
        rule.enabled = false;
        let rules = vec![rule];
        let compiled = CompiledRules::new(&rules, &[]);
        let k = key(r"C:\Chat\chat.exe");
        assert_eq!(
            compiled.eligibility(&AppFacts::new(&k), &rules),
            Eligibility::Disabled(0)
        );
        let other = key(r"C:\Other\other.exe");
        assert!(compiled.eligibility(&AppFacts::new(&other), &rules).is_eligible());
    }

    #[test]
    fn same_target_detects_equivalent_matchers() {
        assert!(AppMatcher::path(r"C:\A\b.exe").same_target(&AppMatcher::path("c:/a/B.EXE")));
        assert!(!AppMatcher::path(r"C:\A\b.exe").same_target(&AppMatcher::exe_name("b.exe")));
    }

    #[test]
    fn rule_ids_are_prefixed_and_unique_enough() {
        let a = new_rule_id("r");
        let b = new_rule_id("r");
        assert!(a.starts_with("r-"));
        assert_eq!(a.len(), 18);
        assert_ne!(a, b);
    }
}
