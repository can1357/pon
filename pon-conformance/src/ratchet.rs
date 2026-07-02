use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::scoreboard::{Scoreboard, Status};
use crate::suite::CPYTHON_TAG;

const FLOOR_FILE: &str = "conformance-floor.json";
/// Floor file for `--suite cpython-full` (workspace root, sibling of `conformance-floor.json`).
pub const FULL_FLOOR_FILE: &str = "conformance-full-floor.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Floor {
    pub cpython_tag: String,
    pub passing_modules: Vec<String>,
    pub min_pass_count: usize,
}

impl Floor {
    pub fn read_or_default(root: &Path) -> Result<Self> {
        Self::read_or_default_at(root, FLOOR_FILE)
    }

    /// `read_or_default` twin for an arbitrary floor file name under `root`.
    pub fn read_or_default_at(root: &Path, file: &str) -> Result<Self> {
        let path = root.join(file);
        if !path.is_file() {
            return Ok(Self::default_floor());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("failed to read `{}`", path.display()))?;
        parse_floor(&text).with_context(|| format!("failed to parse `{}`", path.display()))
    }

    pub fn default_floor() -> Self {
        Self {
            cpython_tag: CPYTHON_TAG.to_owned(),
            passing_modules: Vec::new(),
            min_pass_count: 0,
        }
    }

    fn to_json(&self) -> String {
        let mut modules = self.passing_modules.clone();
        modules.sort();
        modules.dedup();

        let mut json = String::new();
        json.push_str("{\n");
        json.push_str(&format!("  \"cpython_tag\": \"{}\",\n", escape_json(&self.cpython_tag)));
        json.push_str("  \"passing_modules\": [");
        if !modules.is_empty() {
            json.push('\n');
        }
        for (index, module) in modules.iter().enumerate() {
            let comma = if index + 1 == modules.len() { "" } else { "," };
            json.push_str(&format!("    \"{}\"{comma}\n", escape_json(module)));
        }
        json.push_str("  ],\n");
        json.push_str(&format!("  \"min_pass_count\": {}\n", self.min_pass_count));
        json.push_str("}\n");
        json
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FloorCheck {
    pub regressed_modules: Vec<String>,
    pub pass_count: usize,
    pub min_pass_count: usize,
}

impl FloorCheck {
    pub fn is_ok(&self) -> bool {
        self.regressed_modules.is_empty() && self.pass_count >= self.min_pass_count
    }

    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if !self.regressed_modules.is_empty() {
            parts.push(format!(
                "floor module(s) no longer passing: {}",
                self.regressed_modules.join(", ")
            ));
        }
        if self.pass_count < self.min_pass_count {
            parts.push(format!(
                "pass count {} below floor {}",
                self.pass_count, self.min_pass_count
            ));
        }
        if parts.is_empty() {
            "conformance floor satisfied".to_owned()
        } else {
            format!("conformance floor regressed: {}", parts.join("; "))
        }
    }
}

pub fn check_floor(floor: &Floor, scoreboard: &Scoreboard) -> FloorCheck {
    let regressed_modules = floor
        .passing_modules
        .iter()
        .filter(|module| scoreboard.status_for_module(module) != Some(Status::Pass))
        .cloned()
        .collect::<Vec<_>>();

    FloorCheck {
        regressed_modules,
        pass_count: scoreboard.pass_count(),
        min_pass_count: floor.min_pass_count,
    }
}

pub fn write_floor_from_scoreboard(root: &Path, cpython_tag: &str, scoreboard: &Scoreboard) -> Result<()> {
    write_floor_from_scoreboard_at(root, FLOOR_FILE, cpython_tag, scoreboard)
}

/// `write_floor_from_scoreboard` twin for an arbitrary floor file name under `root`.
pub fn write_floor_from_scoreboard_at(root: &Path, file: &str, cpython_tag: &str, scoreboard: &Scoreboard) -> Result<()> {
    let passing_modules = scoreboard.passing_modules();
    let floor = Floor {
        cpython_tag: cpython_tag.to_owned(),
        min_pass_count: passing_modules.len(),
        passing_modules,
    };
    let path = root.join(file);
    fs::write(&path, floor.to_json()).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Renders the `--diff-floor` report (pin J0.7 §8.3): a summary line, then all
/// `regressed` lines (floor modules whose status in this run is not `pass`,
/// absent counting as regressed), then all `progressed` lines (`pass` modules
/// not in the floor's set), each group sorted.
pub fn diff_floor(floor: &Floor, scoreboard: &Scoreboard) -> String {
    let mut regressed = floor
        .passing_modules
        .iter()
        .filter(|module| scoreboard.status_for_module(module) != Some(Status::Pass))
        .cloned()
        .collect::<Vec<_>>();
    regressed.sort();
    regressed.dedup();

    let floor_set = floor.passing_modules.iter().collect::<std::collections::BTreeSet<_>>();
    let mut progressed = scoreboard
        .passing_modules()
        .into_iter()
        .filter(|module| !floor_set.contains(module))
        .collect::<Vec<_>>();
    progressed.sort();

    let mut report = format!(
        "floor-diff {}: {} regressed, {} progressed\n",
        scoreboard.suite,
        regressed.len(),
        progressed.len()
    );
    for module in &regressed {
        report.push_str(&format!("floor-diff regressed {module}\n"));
    }
    for module in &progressed {
        report.push_str(&format!("floor-diff progressed {module}\n"));
    }
    report
}

fn parse_floor(text: &str) -> Result<Floor> {
    let cpython_tag = parse_string_field(text, "cpython_tag")?;
    let passing_modules = parse_string_array_field(text, "passing_modules")?;
    let min_pass_count = parse_usize_field(text, "min_pass_count")?;
    Ok(Floor {
        cpython_tag,
        passing_modules,
        min_pass_count,
    })
}

fn parse_string_field(text: &str, key: &str) -> Result<String> {
    let value = field_value(text, key)?;
    let start = skip_ws(value.as_bytes(), 0);
    let (parsed, _) = parse_json_string(&value[start..])?;
    Ok(parsed)
}

fn parse_string_array_field(text: &str, key: &str) -> Result<Vec<String>> {
    let value = field_value(text, key)?;
    let bytes = value.as_bytes();
    let mut index = skip_ws(bytes, 0);
    if bytes.get(index) != Some(&b'[') {
        bail!("field `{key}` must be a string array")
    }
    index += 1;

    let mut values = Vec::new();
    loop {
        index = skip_ws(bytes, index);
        match bytes.get(index) {
            Some(b']') => return Ok(values),
            Some(b'\"') => {
                let (parsed, consumed) = parse_json_string(&value[index..])?;
                values.push(parsed);
                index += consumed;
                index = skip_ws(bytes, index);
                match bytes.get(index) {
                    Some(b',') => index += 1,
                    Some(b']') => return Ok(values),
                    _ => bail!("field `{key}` array is not comma-delimited"),
                }
            }
            _ => bail!("field `{key}` must contain strings"),
        }
    }
}

fn parse_usize_field(text: &str, key: &str) -> Result<usize> {
    let value = field_value(text, key)?;
    let bytes = value.as_bytes();
    let start = skip_ws(bytes, 0);
    let mut end = start;
    while matches!(bytes.get(end), Some(b'0'..=b'9')) {
        end += 1;
    }
    if start == end {
        bail!("field `{key}` must be an unsigned integer")
    }
    value[start..end]
        .parse::<usize>()
        .with_context(|| format!("field `{key}` did not fit in usize"))
}

fn field_value<'a>(text: &'a str, key: &str) -> Result<&'a str> {
    let needle = format!("\"{key}\"");
    let key_start = text.find(&needle).with_context(|| format!("missing field `{key}`"))?;
    let after_key = &text[key_start + needle.len()..];
    let colon = after_key.find(':').with_context(|| format!("missing colon after field `{key}`"))?;
    Ok(&after_key[colon + 1..])
}

fn parse_json_string(value: &str) -> Result<(String, usize)> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'\"') {
        bail!("expected JSON string")
    }

    let mut parsed = String::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\"' => return Ok((parsed, index + 1)),
            b'\\' => {
                index += 1;
                match bytes.get(index).copied() {
                    Some(b'\"') => parsed.push('"'),
                    Some(b'\\') => parsed.push('\\'),
                    Some(b'/') => parsed.push('/'),
                    Some(b'b') => parsed.push('\u{0008}'),
                    Some(b'f') => parsed.push('\u{000c}'),
                    Some(b'n') => parsed.push('\n'),
                    Some(b'r') => parsed.push('\r'),
                    Some(b't') => parsed.push('\t'),
                    Some(b'u') => {
                        let digits_start = index + 1;
                        let digits_end = digits_start + 4;
                        if digits_end > bytes.len() {
                            bail!("unterminated unicode escape")
                        }
                        let digits = &value[digits_start..digits_end];
                        let code = u32::from_str_radix(digits, 16).context("invalid unicode escape")?;
                        let character = char::from_u32(code).context("unicode escape is not a scalar value")?;
                        parsed.push(character);
                        index = digits_end - 1;
                    }
                    _ => bail!("invalid JSON escape"),
                }
                index += 1;
            }
            _ => {
                let character = value[index..]
                    .chars()
                    .next()
                    .context("string ended in the middle of a character")?;
                parsed.push(character);
                index += character.len_utf8();
            }
        }
    }

    bail!("unterminated JSON string")
}

fn skip_ws(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        index += 1;
    }
    index
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => escaped.push_str(&format!("\\u{:04x}", character as u32)),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoreboard::Status;

    #[test]
    fn synthetic_floor_regression_catches_module_and_count_drops() {
        let floor = Floor {
            cpython_tag: "v3.14.0".to_owned(),
            passing_modules: vec!["Lib/test/test_a.py".to_owned(), "Lib/test/test_b.py".to_owned()],
            min_pass_count: 2,
        };
        let mut scoreboard = Scoreboard::new("cpython", Some("v3.14.0".to_owned()));
        scoreboard.push("Lib/test/test_a.py", Status::Pass, None);
        scoreboard.push("Lib/test/test_b.py", Status::SemanticsDivergent, Some("different stdout".to_owned()));

        let report = check_floor(&floor, &scoreboard);

        assert!(!report.is_ok());
        assert_eq!(report.regressed_modules, vec!["Lib/test/test_b.py"]);
        assert_eq!(report.pass_count, 1);
        assert!(report.message().contains("pass count 1 below floor 2"));
    }

    #[test]
    fn diff_floor_reports_regressed_then_progressed_sorted() {
        let floor = Floor {
            cpython_tag: "v3.14.0".to_owned(),
            passing_modules: vec!["test.test_b".to_owned(), "test.test_a".to_owned()],
            min_pass_count: 2,
        };
        let mut scoreboard = Scoreboard::new("cpython-full", Some("v3.14.0".to_owned()));
        scoreboard.push("test.test_a", Status::Pass, None);
        scoreboard.push("test.test_d", Status::Pass, None);
        scoreboard.push("test.test_c", Status::Pass, None);
        // test.test_b absent from this run: counts as regressed.

        let report = diff_floor(&floor, &scoreboard);

        assert_eq!(
            report,
            "floor-diff cpython-full: 1 regressed, 2 progressed\n\
             floor-diff regressed test.test_b\n\
             floor-diff progressed test.test_c\n\
             floor-diff progressed test.test_d\n"
        );
    }
}
