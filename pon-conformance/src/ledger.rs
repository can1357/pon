//! Divergence/exclusion ledgers for `--suite cpython-full`.
//!
//! Contract: `plans/pon-pin-J07-cpython-full-runner.md` §7 (frozen). Both files
//! are parsed with a hand-rolled reader for the pinned TOML subset (§7.4):
//! `#` comments, blank lines, `[[divergence]]` / `[[exclude]]` array-of-tables
//! headers, `key = "string"` with `\"`/`\\` escapes, and single-line string
//! arrays. Anything else is a load error.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Hard cap on `[[divergence]]` entries (pin §7.1).
pub const MAX_DIVERGENCE_ENTRIES: usize = 25;
/// Hard cap on `[[exclude]]` entries (pin §7.1).
pub const MAX_EXCLUSION_ENTRIES: usize = 40;

const DIVERGENCE_FILE: &str = "pon-conformance/divergence-ledger.toml";
const EXCLUSIONS_FILE: &str = "pon-conformance/exclusions.toml";

/// Closed reason taxonomy for `divergence-ledger.toml` (pin §7.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DivergenceReason {
    RefcountObservability,
    DelTiming,
    WeakrefTiming,
}

impl DivergenceReason {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "refcount-observability" => Some(Self::RefcountObservability),
            "del-timing" => Some(Self::DelTiming),
            "weakref-timing" => Some(Self::WeakrefTiming),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RefcountObservability => "refcount-observability",
            Self::DelTiming => "del-timing",
            Self::WeakrefTiming => "weakref-timing",
        }
    }
}

impl fmt::Display for DivergenceReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Closed reason taxonomy for `exclusions.toml` (pin §7.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExcludeReason {
    CAbiBoundary,
    PlatformInapplicable,
    InterpreterInternals,
}

impl ExcludeReason {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "c-abi-boundary" => Some(Self::CAbiBoundary),
            "platform-inapplicable" => Some(Self::PlatformInapplicable),
            "interpreter-internals" => Some(Self::InterpreterInternals),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CAbiBoundary => "c-abi-boundary",
            Self::PlatformInapplicable => "platform-inapplicable",
            Self::InterpreterInternals => "interpreter-internals",
        }
    }
}

impl fmt::Display for ExcludeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `[[divergence]]` entry (pin §7.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DivergenceEntry {
    pub pattern: String,
    pub reason: DivergenceReason,
    /// Observed CPython behavior.
    pub cpython: String,
    /// pon behavior + why the product boundary forces it.
    pub pon: String,
    /// Empty = every differing id in a matched unit is covered.
    pub test_ids: Vec<String>,
    pub approved_by: String,
    pub note: String,
}

/// One `[[exclude]]` entry (pin §7.4).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludeEntry {
    pub pattern: String,
    pub reason: ExcludeReason,
    pub note: String,
    pub approved_by: String,
}

/// Both ledgers, loaded and validated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ledger {
    pub divergences: Vec<DivergenceEntry>,
    pub exclusions: Vec<ExcludeEntry>,
}

impl Ledger {
    /// First `[[exclude]]` entry matching the unit (file order wins).
    pub fn exclusion_for(&self, key: &str, stem: &str) -> Option<&ExcludeEntry> {
        self.exclusions.iter().find(|entry| pattern_matches_unit(&entry.pattern, key, stem))
    }

    /// First `[[divergence]]` entry matching the unit (file order wins).
    pub fn divergence_for(&self, key: &str, stem: &str) -> Option<&DivergenceEntry> {
        self.divergences.iter().find(|entry| pattern_matches_unit(&entry.pattern, key, stem))
    }
}

/// Loads and validates both ledger files at the crate root under `root`.
pub fn load_ledger(root: &Path) -> Result<Ledger> {
    let divergence_path = root.join(DIVERGENCE_FILE);
    let divergence_text = std::fs::read_to_string(&divergence_path)
        .with_context(|| format!("failed to read `{}`", divergence_path.display()))?;
    let divergences =
        parse_divergences(&divergence_text).with_context(|| format!("failed to load `{}`", divergence_path.display()))?;

    let exclusions_path = root.join(EXCLUSIONS_FILE);
    let exclusions_text = std::fs::read_to_string(&exclusions_path)
        .with_context(|| format!("failed to read `{}`", exclusions_path.display()))?;
    let exclusions =
        parse_exclusions(&exclusions_text).with_context(|| format!("failed to load `{}`", exclusions_path.display()))?;

    Ok(Ledger { divergences, exclusions })
}

/// Pinned glob dialect (§7.3): `*` matches any run of characters *including*
/// `.`; `?` matches exactly one character; everything else is literal.
pub fn glob_match(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let candidate = candidate.chars().collect::<Vec<_>>();
    glob_match_at(&pattern, &candidate)
}

fn glob_match_at(pattern: &[char], candidate: &[char]) -> bool {
    // Iterative backtracking matcher: only `*` introduces choice points, and
    // greedily re-anchoring the last `*` is complete for this dialect.
    let (mut p, mut c) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;

    while c < candidate.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == candidate[c]) {
            p += 1;
            c += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, c));
            p += 1;
        } else if let Some((star_p, star_c)) = star {
            p = star_p + 1;
            c = star_c + 1;
            star = Some((star_p, star_c + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// A pattern matches a unit iff it matches its match key *or* its stem (pin §7.3).
pub fn pattern_matches_unit(pattern: &str, key: &str, stem: &str) -> bool {
    glob_match(pattern, key) || glob_match(pattern, stem)
}

// ---------------------------------------------------------------------------
// TOML-subset reader (pin §7.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum TomlValue {
    String(String),
    Array(Vec<String>),
}

#[derive(Clone, Debug, Default)]
struct TomlTable {
    line: usize,
    fields: Vec<(String, TomlValue)>,
}

impl TomlTable {
    fn take_string(&self, key: &str) -> Option<&str> {
        self.fields.iter().find_map(|(name, value)| match value {
            TomlValue::String(text) if name == key => Some(text.as_str()),
            _ => None,
        })
    }

    fn take_array(&self, key: &str) -> Option<&[String]> {
        self.fields.iter().find_map(|(name, value)| match value {
            TomlValue::Array(items) if name == key => Some(items.as_slice()),
            _ => None,
        })
    }

    fn required_string(&self, key: &str) -> Result<String> {
        match self.take_string(key) {
            Some(text) if !text.is_empty() => Ok(text.to_owned()),
            Some(_) => bail!("line {}: field `{key}` must be non-empty", self.line),
            None => bail!("line {}: missing required field `{key}`", self.line),
        }
    }
}

/// Splits the file into array-of-tables entries under the given header name,
/// enforcing the pinned subset grammar.
fn parse_tables(text: &str, header: &str) -> Result<Vec<TomlTable>> {
    let expected_header = format!("[[{header}]]");
    let mut tables: Vec<TomlTable> = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("[[") {
            if line != expected_header {
                bail!("line {line_no}: unsupported table header `{line}` (only `{expected_header}` is allowed)");
            }
            tables.push(TomlTable {
                line: line_no,
                fields: Vec::new(),
            });
            continue;
        }
        let Some(table) = tables.last_mut() else {
            bail!("line {line_no}: key/value pair before any `{expected_header}` header");
        };
        let (key, value) = parse_key_value(line, line_no)?;
        if table.fields.iter().any(|(name, _)| *name == key) {
            bail!("line {line_no}: duplicate key `{key}`");
        }
        table.fields.push((key, value));
    }

    Ok(tables)
}

fn parse_key_value(line: &str, line_no: usize) -> Result<(String, TomlValue)> {
    let Some((key, rest)) = line.split_once('=') else {
        bail!("line {line_no}: expected `key = value`");
    };
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("line {line_no}: invalid key `{key}`");
    }
    let rest = rest.trim_start();
    let (value, consumed) = if rest.starts_with('"') {
        let (text, consumed) = parse_toml_string(rest, line_no)?;
        (TomlValue::String(text), consumed)
    } else if rest.starts_with('[') {
        let (items, consumed) = parse_toml_string_array(rest, line_no)?;
        (TomlValue::Array(items), consumed)
    } else {
        bail!("line {line_no}: value must be a double-quoted string or a string array");
    };
    let trailer = rest[consumed..].trim_start();
    if !trailer.is_empty() && !trailer.starts_with('#') {
        bail!("line {line_no}: unexpected trailing content `{trailer}`");
    }
    Ok((key.to_owned(), value))
}

/// Parses one `"..."` with `\"` and `\\` escapes only; returns (text, bytes consumed).
fn parse_toml_string(text: &str, line_no: usize) -> Result<(String, usize)> {
    debug_assert!(text.starts_with('"'));
    let mut parsed = String::new();
    let mut chars = text.char_indices().skip(1);
    while let Some((index, character)) = chars.next() {
        match character {
            '"' => return Ok((parsed, index + 1)),
            '\\' => match chars.next() {
                Some((_, '"')) => parsed.push('"'),
                Some((_, '\\')) => parsed.push('\\'),
                Some((_, other)) => bail!("line {line_no}: unsupported escape `\\{other}` (only \\\" and \\\\)"),
                None => bail!("line {line_no}: unterminated escape"),
            },
            other => parsed.push(other),
        }
    }
    bail!("line {line_no}: unterminated string")
}

/// Parses one single-line `["a", "b"]` string array; returns (items, bytes consumed).
fn parse_toml_string_array(text: &str, line_no: usize) -> Result<(Vec<String>, usize)> {
    debug_assert!(text.starts_with('['));
    let mut items = Vec::new();
    let mut index = 1usize;
    let bytes = text.as_bytes();
    loop {
        while matches!(bytes.get(index), Some(b' ' | b'\t')) {
            index += 1;
        }
        match bytes.get(index) {
            Some(b']') => return Ok((items, index + 1)),
            Some(b'"') => {
                let (item, consumed) = parse_toml_string(&text[index..], line_no)?;
                items.push(item);
                index += consumed;
                while matches!(bytes.get(index), Some(b' ' | b'\t')) {
                    index += 1;
                }
                match bytes.get(index) {
                    Some(b',') => index += 1,
                    Some(b']') => return Ok((items, index + 1)),
                    _ => bail!("line {line_no}: string array is not comma-delimited"),
                }
            }
            _ => bail!("line {line_no}: string array must contain only double-quoted strings on one line"),
        }
    }
}

fn parse_divergences(text: &str) -> Result<Vec<DivergenceEntry>> {
    let tables = parse_tables(text, "divergence")?;
    if tables.len() > MAX_DIVERGENCE_ENTRIES {
        bail!("{} [[divergence]] entries exceed the cap of {MAX_DIVERGENCE_ENTRIES}", tables.len());
    }
    const KNOWN_KEYS: &[&str] = &["pattern", "reason", "cpython", "pon", "test_ids", "approved_by", "note"];
    tables
        .iter()
        .map(|table| {
            reject_unknown_keys(table, KNOWN_KEYS)?;
            let reason_text = table.required_string("reason")?;
            let Some(reason) = DivergenceReason::parse(&reason_text) else {
                bail!(
                    "line {}: reason `{reason_text}` is outside the closed taxonomy (refcount-observability | del-timing | weakref-timing)",
                    table.line
                );
            };
            Ok(DivergenceEntry {
                pattern: table.required_string("pattern")?,
                reason,
                cpython: table.required_string("cpython")?,
                pon: table.required_string("pon")?,
                test_ids: table.take_array("test_ids").map(<[String]>::to_vec).unwrap_or_default(),
                approved_by: table.required_string("approved_by")?,
                note: table.take_string("note").unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

fn parse_exclusions(text: &str) -> Result<Vec<ExcludeEntry>> {
    let tables = parse_tables(text, "exclude")?;
    if tables.len() > MAX_EXCLUSION_ENTRIES {
        bail!("{} [[exclude]] entries exceed the cap of {MAX_EXCLUSION_ENTRIES}", tables.len());
    }
    const KNOWN_KEYS: &[&str] = &["pattern", "reason", "note", "approved_by"];
    tables
        .iter()
        .map(|table| {
            reject_unknown_keys(table, KNOWN_KEYS)?;
            let reason_text = table.required_string("reason")?;
            let Some(reason) = ExcludeReason::parse(&reason_text) else {
                bail!(
                    "line {}: reason `{reason_text}` is outside the closed taxonomy (c-abi-boundary | platform-inapplicable | interpreter-internals)",
                    table.line
                );
            };
            Ok(ExcludeEntry {
                pattern: table.required_string("pattern")?,
                reason,
                note: table.required_string("note")?,
                approved_by: table.required_string("approved_by")?,
            })
        })
        .collect()
}

fn reject_unknown_keys(table: &TomlTable, known: &[&str]) -> Result<()> {
    for (key, _) in &table.fields {
        if !known.contains(&key.as_str()) {
            bail!("line {}: unknown key `{key}`", table.line);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_dialect_matches_pin_table() {
        // Pin §7.3 worked table.
        assert!(pattern_matches_unit("test_dis", "test_dis", "test_dis"));
        assert!(!pattern_matches_unit("test_dis", "test_ctypes.test_numbers", "test_numbers"));
        assert!(!pattern_matches_unit("test_ctypes*", "test_dis", "test_dis"));
        assert!(pattern_matches_unit("test_ctypes*", "test_ctypes.test_numbers", "test_numbers"));
        assert!(!pattern_matches_unit("test_numbers", "test_dis", "test_dis"));
        assert!(pattern_matches_unit("test_numbers", "test_ctypes.test_numbers", "test_numbers"));

        // `*` crosses `.`; `?` is exactly one char.
        assert!(glob_match("test_json*", "test_json.test_decode"));
        assert!(glob_match("test_*.test_*", "test_json.test_decode"));
        assert!(glob_match("test_di?", "test_dis"));
        assert!(!glob_match("test_di?", "test_di"));
        assert!(glob_match("*", "anything.at.all"));
        assert!(!glob_match("test_json", "test_json2"));
    }

    #[test]
    fn toml_subset_parses_entries_and_rejects_junk() {
        let text = r#"
# comment
[[exclude]]
pattern     = "test_ctypes*"   # trailing comment
reason      = "c-abi-boundary"
note        = "quoted \"note\" with back\\slash"
approved_by = "J0-pin"
"#;
        let entries = parse_exclusions(text).expect("subset parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pattern, "test_ctypes*");
        assert_eq!(entries[0].reason, ExcludeReason::CAbiBoundary);
        assert_eq!(entries[0].note, "quoted \"note\" with back\\slash");

        assert!(parse_exclusions("[[exclude]]\npattern = \"x\"\nreason = \"nope\"\nnote = \"n\"\napproved_by = \"a\"").is_err());
        assert!(parse_exclusions("[[exclude]]\npattern = \"x\"\nreason = \"c-abi-boundary\"\nnote = \"n\"\napproved_by = \"a\"\nbogus = \"y\"").is_err());
        assert!(parse_exclusions("[[wrong]]\n").is_err());
        assert!(parse_exclusions("pattern = \"x\"\n").is_err());
        assert!(parse_exclusions("[[exclude]]\npattern = 42\n").is_err());
    }

    #[test]
    fn divergence_entries_parse_test_ids_arrays() {
        let text = r#"
[[divergence]]
pattern     = "test_gc*"
reason      = "refcount-observability"
cpython     = "refcounted temporaries die at scope exit"
pon         = "tracing GC frees later"
test_ids    = ["test.test_gc.GCTests.test_a", "test.test_gc.GCTests.test_b"]
approved_by = "orchestrator"
"#;
        let entries = parse_divergences(text).expect("subset parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].test_ids.len(), 2);
        assert_eq!(entries[0].reason, DivergenceReason::RefcountObservability);
        assert!(entries[0].note.is_empty());

        // Missing required field is a load error.
        let missing = "[[divergence]]\npattern = \"p\"\nreason = \"del-timing\"\ncpython = \"c\"\napproved_by = \"a\"";
        assert!(parse_divergences(missing).is_err());
    }

    #[test]
    fn checked_in_ledgers_load_and_carry_seed_exclusions() {
        let root = crate::suite::workspace_root();
        let ledger = load_ledger(&root).expect("checked-in ledgers load");
        assert!(ledger.divergences.is_empty(), "J0 divergence ledger starts empty");
        let patterns = ledger.exclusions.iter().map(|entry| entry.pattern.as_str()).collect::<Vec<_>>();
        assert_eq!(patterns, ["test_ctypes*", "test_capi*", "test_dis", "test_sys_settrace"]);
        assert!(ledger.exclusion_for("test_capi.test_abstract", "test_abstract").is_some());
        assert!(ledger.exclusion_for("test_grammar", "test_grammar").is_none());
    }
}
