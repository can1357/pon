//! Pip-compatible requirements-file parsing for `pon`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::requirement::{RequirementInput, parse_requirement_input};

/// Parsed contents of a pip-style requirements file.
///
/// Recursive `-r` includes are flattened into this structure in encounter order.
/// Requirement entries retain their source file and line so later resolver and CLI
/// phases can produce diagnostics against the original input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RequirementsFile {
    /// Concrete requirements requested by the file and any recursive includes.
    pub entries: Vec<RequirementEntry>,
    /// Constraint files named by `-c`/`--constraint`, resolved relative to the
    /// file that mentioned them.
    pub constraints: Vec<PathBuf>,
    /// Last `-i`/`--index-url` value encountered while parsing the file tree.
    pub index_url: Option<String>,
    /// `--extra-index-url` values in encounter order.
    pub extra_index_urls: Vec<String>,
    /// Whether `--no-index` appeared anywhere in the file tree.
    pub no_index: bool,
    /// Whether prereleases are allowed via `--pre`.
    pub pre: bool,
    /// Whether hash-checking mode was requested via `--require-hashes`.
    pub require_hashes: bool,
}

/// One installable requirement line from a requirements file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequirementEntry {
    /// Parsed requirement input after comment stripping, continuation joining,
    /// environment expansion, and relative path resolution.
    pub input: RequirementInput,
    /// Repeated `--hash=algo:hex` values attached to this requirement.
    pub hashes: Vec<String>,
    /// One-based physical line number where this logical requirement starts.
    pub line: usize,
    /// Requirements file that contributed this entry.
    pub file: PathBuf,
}

/// Parse a pip-style requirements file.
///
/// Supported grammar: `#` comments at line start or after whitespace, trailing
/// backslash continuations, `${VAR}` environment expansion with undefined vars
/// left verbatim, recursive `-r` includes, `-c` constraints, editable local
/// directories, index options, `--no-index`, `--pre`, `--require-hashes`, and
/// repeated per-requirement `--hash=algo:hex` options.
pub fn parse_requirements_file(path: &Path) -> Result<RequirementsFile> {
    let mut stack = Vec::new();
    parse_requirements_file_inner(path, &mut stack)
}

fn parse_requirements_file_inner(path: &Path, stack: &mut Vec<PathBuf>) -> Result<RequirementsFile> {
    let cycle_key = cycle_key(path);
    if stack.iter().any(|active| active == &cycle_key) {
        return Err(Error::Cli(format!("circular -r include: {}", path.display())));
    }

    stack.push(cycle_key);
    let parsed = parse_requirements_file_uncycled(path, stack);
    stack.pop();
    parsed
}

fn parse_requirements_file_uncycled(path: &Path, stack: &mut Vec<PathBuf>) -> Result<RequirementsFile> {
    let content = fs::read_to_string(path)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = RequirementsFile::default();

    for logical in logical_lines(&content) {
        let expanded = expand_environment(&logical.text);
        parse_logical_line(&expanded, logical.line, path, base_dir, stack, &mut file)?;
    }

    Ok(file)
}

fn parse_logical_line(
    line: &str,
    line_number: usize,
    file_path: &Path,
    base_dir: &Path,
    stack: &mut Vec<PathBuf>,
    file: &mut RequirementsFile,
) -> Result<()> {
    let tokens = tokenize(line);
    let Some(first) = tokens.first() else {
        return Ok(());
    };

    match option_name_and_value(&first.text, "--requirement") {
        OptionMatch::Flag => {
            let include = required_option_value(&tokens, 0, "--requirement", file_path, line_number)?;
            reject_extra_tokens(&tokens, 2, file_path, line_number)?;
            parse_include(include, base_dir, stack, file)
        }
        OptionMatch::Inline(include) => {
            let include = require_inline_value(include, "--requirement", file_path, line_number)?;
            reject_extra_tokens(&tokens, 1, file_path, line_number)?;
            parse_include(include, base_dir, stack, file)
        }
        OptionMatch::NoMatch => match option_name_and_value(&first.text, "--constraint") {
            OptionMatch::Flag => {
                let constraint = required_option_value(&tokens, 0, "--constraint", file_path, line_number)?;
                reject_extra_tokens(&tokens, 2, file_path, line_number)?;
                file.constraints.push(resolve_relative_path(base_dir, constraint));
                Ok(())
            }
            OptionMatch::Inline(constraint) => {
                let constraint = require_inline_value(constraint, "--constraint", file_path, line_number)?;
                reject_extra_tokens(&tokens, 1, file_path, line_number)?;
                file.constraints.push(resolve_relative_path(base_dir, constraint));
                Ok(())
            }
            OptionMatch::NoMatch => parse_non_include_line(line, &tokens, line_number, file_path, base_dir, stack, file),
        },
    }
}

fn parse_non_include_line(
    line: &str,
    tokens: &[Token],
    line_number: usize,
    file_path: &Path,
    base_dir: &Path,
    stack: &mut Vec<PathBuf>,
    file: &mut RequirementsFile,
) -> Result<()> {
    let first = &tokens[0].text;

    if first == "-r" {
        let include = required_option_value(tokens, 0, "-r", file_path, line_number)?;
        reject_extra_tokens(tokens, 2, file_path, line_number)?;
        return parse_include(include, base_dir, stack, file);
    }
    if first == "-c" {
        let constraint = required_option_value(tokens, 0, "-c", file_path, line_number)?;
        reject_extra_tokens(tokens, 2, file_path, line_number)?;
        file.constraints.push(resolve_relative_path(base_dir, constraint));
        return Ok(());
    }
    if first == "-e" || first == "--editable" {
        let editable = required_option_value(tokens, 0, first, file_path, line_number)?;
        reject_extra_tokens(tokens, 2, file_path, line_number)?;
        file.entries.push(parse_editable_entry(editable, line_number, file_path, base_dir)?);
        return Ok(());
    }
    if let OptionMatch::Inline(editable) = option_name_and_value(first, "--editable") {
        let editable = require_inline_value(editable, "--editable", file_path, line_number)?;
        reject_extra_tokens(tokens, 1, file_path, line_number)?;
        file.entries.push(parse_editable_entry(editable, line_number, file_path, base_dir)?);
        return Ok(());
    }
    if first.starts_with('-') {
        return parse_global_options(tokens, line_number, file_path, file);
    }

    file.entries.push(parse_requirement_entry(line, tokens, line_number, file_path, base_dir)?);
    Ok(())
}

fn parse_include(
    raw_path: &str,
    base_dir: &Path,
    stack: &mut Vec<PathBuf>,
    file: &mut RequirementsFile,
) -> Result<()> {
    let include_path = resolve_relative_path(base_dir, raw_path);
    let included = parse_requirements_file_inner(&include_path, stack)?;
    merge_requirements(file, included);
    Ok(())
}

fn parse_global_options(
    tokens: &[Token],
    line_number: usize,
    file_path: &Path,
    file: &mut RequirementsFile,
) -> Result<()> {
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index].text;
        match token.as_str() {
            "--no-index" => {
                file.no_index = true;
                index += 1;
            }
            "--pre" => {
                file.pre = true;
                index += 1;
            }
            "--require-hashes" => {
                file.require_hashes = true;
                index += 1;
            }
            "-i" | "--index-url" => {
                file.index_url = Some(required_option_value(tokens, index, token, file_path, line_number)?.to_owned());
                index += 2;
            }
            "--extra-index-url" => {
                file.extra_index_urls
                    .push(required_option_value(tokens, index, token, file_path, line_number)?.to_owned());
                index += 2;
            }
            _ => match option_name_and_value(token, "--index-url") {
                OptionMatch::Inline(value) => {
                    let value = require_inline_value(value, "--index-url", file_path, line_number)?;
                    file.index_url = Some(value.to_owned());
                    index += 1;
                }
                OptionMatch::NoMatch | OptionMatch::Flag => match option_name_and_value(token, "--extra-index-url") {
                    OptionMatch::Inline(value) => {
                        let value = require_inline_value(value, "--extra-index-url", file_path, line_number)?;
                        file.extra_index_urls.push(value.to_owned());
                        index += 1;
                    }
                    OptionMatch::NoMatch | OptionMatch::Flag => {
                        return Err(unsupported_option(token, file_path, line_number));
                    }
                },
            },
        }
    }

    Ok(())
}

fn parse_requirement_entry(
    line: &str,
    tokens: &[Token],
    line_number: usize,
    file_path: &Path,
    base_dir: &Path,
) -> Result<RequirementEntry> {
    let option_index = tokens.iter().position(|token| !token.quoted && token.text.starts_with('-'));
    let requirement_raw = option_index.map_or_else(
        || line.trim(),
        |index| line[..tokens[index].start].trim_end(),
    );

    if requirement_raw.is_empty() {
        return Err(Error::InvalidRequirement(requirement_raw.to_owned()));
    }

    let mut hashes = Vec::new();
    if let Some(index) = option_index {
        for token in &tokens[index..] {
            let Some(hash) = token.text.strip_prefix("--hash=") else {
                return Err(unsupported_option(&token.text, file_path, line_number));
            };
            if hash.is_empty() {
                return Err(missing_option_value("--hash", file_path, line_number));
            }
            hashes.push(hash.to_owned());
        }
    }

    Ok(RequirementEntry {
        input: parse_requirement_input_at(requirement_raw, base_dir)?,
        hashes,
        line: line_number,
        file: file_path.to_path_buf(),
    })
}

fn parse_editable_entry(
    raw: &str,
    line_number: usize,
    file_path: &Path,
    base_dir: &Path,
) -> Result<RequirementEntry> {
    let path = resolve_relative_path(base_dir, raw);
    if !path.is_dir() {
        return Err(Error::Cli("editable requirements must be local directories".to_owned()));
    }

    Ok(RequirementEntry {
        input: RequirementInput::Path { path, editable: true },
        hashes: Vec::new(),
        line: line_number,
        file: file_path.to_path_buf(),
    })
}

fn parse_requirement_input_at(raw: &str, base_dir: &Path) -> Result<RequirementInput> {
    if archive_path_exists_relative_to(base_dir, raw) {
        return Ok(RequirementInput::Path {
            path: resolve_relative_path(base_dir, raw),
            editable: false,
        });
    }

    let mut input = parse_requirement_input(raw)?;
    if let RequirementInput::Path { path, .. } = &mut input {
        if !path.is_absolute() {
            *path = base_dir.join(&path);
        }
    }
    Ok(input)
}

fn archive_path_exists_relative_to(base_dir: &Path, raw: &str) -> bool {
    (raw.ends_with(".whl") || raw.ends_with(".tar.gz") || raw.ends_with(".zip"))
        && resolve_relative_path(base_dir, raw).exists()
}

fn merge_requirements(target: &mut RequirementsFile, source: RequirementsFile) {
    target.entries.extend(source.entries);
    target.constraints.extend(source.constraints);
    if source.index_url.is_some() {
        target.index_url = source.index_url;
    }
    target.extra_index_urls.extend(source.extra_index_urls);
    target.no_index |= source.no_index;
    target.pre |= source.pre;
    target.require_hashes |= source.require_hashes;
}

fn required_option_value<'a>(
    tokens: &'a [Token],
    option_index: usize,
    option: &str,
    file_path: &Path,
    line_number: usize,
) -> Result<&'a str> {
    let Some(value) = tokens.get(option_index + 1) else {
        return Err(missing_option_value(option, file_path, line_number));
    };
    if !value.quoted && value.text.starts_with('-') {
        return Err(missing_option_value(option, file_path, line_number));
    }
    Ok(value.text.as_str())
}

fn require_inline_value<'a>(
    value: &'a str,
    option: &str,
    file_path: &Path,
    line_number: usize,
) -> Result<&'a str> {
    if value.is_empty() {
        return Err(missing_option_value(option, file_path, line_number));
    }
    Ok(value)
}

fn missing_option_value(option: &str, file_path: &Path, line_number: usize) -> Error {
    Error::Cli(format!(
        "requirements option `{option}` in {}:{} requires a value",
        file_path.display(),
        line_number
    ))
}

fn reject_extra_tokens(
    tokens: &[Token],
    allowed: usize,
    file_path: &Path,
    line_number: usize,
) -> Result<()> {
    if let Some(token) = tokens.get(allowed) {
        return Err(unsupported_option(&token.text, file_path, line_number));
    }
    Ok(())
}

fn unsupported_option(option: &str, file_path: &Path, line_number: usize) -> Error {
    Error::Cli(format!(
        "unsupported requirements option `{option}` in {}:{}",
        file_path.display(),
        line_number
    ))
}

fn resolve_relative_path(base_dir: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn cycle_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogicalLine {
    text: String,
    line: usize,
}

fn logical_lines(content: &str) -> Vec<LogicalLine> {
    let mut lines = Vec::new();
    let mut buffer = String::new();
    let mut start_line = 1;

    for (offset, physical) in content.lines().enumerate() {
        let line_number = offset + 1;
        let uncommented = strip_comment(physical);
        let trimmed = uncommented.trim_end();

        if trimmed.is_empty() && buffer.is_empty() {
            continue;
        }
        if buffer.is_empty() {
            start_line = line_number;
        }

        if let Some(continued) = trimmed.strip_suffix('\\') {
            append_logical_fragment(&mut buffer, continued);
        } else {
            append_logical_fragment(&mut buffer, trimmed);
            if !buffer.trim().is_empty() {
                lines.push(LogicalLine {
                    text: buffer.trim().to_owned(),
                    line: start_line,
                });
            }
            buffer.clear();
        }
    }

    if !buffer.trim().is_empty() {
        lines.push(LogicalLine {
            text: buffer.trim().to_owned(),
            line: start_line,
        });
    }

    lines
}

fn append_logical_fragment(buffer: &mut String, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    if !buffer.is_empty() {
        buffer.push(' ');
    }
    buffer.push_str(fragment);
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut previous = None;

    for (index, character) in line.char_indices() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            previous = Some(character);
            continue;
        }

        if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '#' && previous.is_none_or(char::is_whitespace) {
            return &line[..index];
        }
        previous = Some(character);
    }

    line
}

fn expand_environment(line: &str) -> String {
    let mut expanded = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let variable_start = start + 2;
        let Some(end) = rest[variable_start..].find('}') else {
            expanded.push_str(&rest[start..]);
            return expanded;
        };
        let variable_end = variable_start + end;
        let name = &rest[variable_start..variable_end];
        match std::env::var(name) {
            Ok(value) => expanded.push_str(&value),
            Err(_) => expanded.push_str(&rest[start..=variable_end]),
        }
        rest = &rest[variable_end + 1..];
    }

    expanded.push_str(rest);
    expanded
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    text: String,
    start: usize,
    quoted: bool,
}

fn tokenize(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut quote = None;
    let mut start = None;
    let mut text = String::new();
    let mut quoted = false;

    for (index, character) in line.char_indices() {
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else {
                text.push(character);
            }
            continue;
        }

        if character == '\'' || character == '"' {
            if start.is_none() {
                start = Some(index);
            }
            quoted = true;
            quote = Some(character);
        } else if character.is_whitespace() {
            if let Some(token_start) = start.take() {
                tokens.push(Token {
                    text: std::mem::take(&mut text),
                    start: token_start,
                    quoted,
                });
                quoted = false;
            }
        } else {
            if start.is_none() {
                start = Some(index);
            }
            text.push(character);
        }
    }

    if let Some(token_start) = start {
        tokens.push(Token {
            text,
            start: token_start,
            quoted,
        });
    }

    tokens
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionMatch<'a> {
    NoMatch,
    Flag,
    Inline(&'a str),
}

fn option_name_and_value<'a>(token: &'a str, name: &str) -> OptionMatch<'a> {
    if token == name {
        OptionMatch::Flag
    } else if let Some(value) = token.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')) {
        OptionMatch::Inline(value)
    } else {
        OptionMatch::NoMatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_options_comments_continuations_env_and_hashes() {
        let root = temp_dir("grammar");
        let file = root.join("requirements.txt");
        let env_key = unique_env_key("INDEX_HOST");
        let unset_key = unique_env_key("UNDEFINED");
        let _guard = EnvGuard::set(&env_key, "packages.example.test");
        fs::write(
            &file,
            format!(
                "# leading comment\n\
                 --index-url https://${{{env_key}}}/simple\n\
                 --extra-index-url https://${{{env_key}}}/extra # trailing comment\n\
                 --no-index --pre --require-hashes\n\
                 demo>=1 \\\n\
                     --hash=sha256:abc --hash=sha256:def # hash comment\n\
                 git+https://example.test/org/demo.git#subdirectory=pkg\n\
                 --index-url ${{{unset_key}}}\n"
            ),
        )
        .expect("write requirements");

        let parsed = parse_requirements_file(&file).expect("parse requirements");

        let unset_reference = format!("${{{unset_key}}}");
        assert_eq!(parsed.index_url.as_deref(), Some(unset_reference.as_str()));
        assert_eq!(parsed.extra_index_urls, vec!["https://packages.example.test/extra"]);
        assert!(parsed.no_index);
        assert!(parsed.pre);
        assert!(parsed.require_hashes);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].hashes, vec!["sha256:abc", "sha256:def"]);
        assert_eq!(parsed.entries[0].line, 5);
        assert!(matches!(parsed.entries[0].input, RequirementInput::Pep508(_)));
        assert!(matches!(parsed.entries[1].input, RequirementInput::Url { .. }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn flattens_recursive_includes_and_resolves_constraint_paths() {
        let root = temp_dir("includes");
        let nested_dir = root.join("nested");
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        let main = root.join("requirements.txt");
        let nested = nested_dir.join("dev.txt");
        fs::write(&main, "-r nested/dev.txt\nrootpkg==1\n").expect("write main");
        fs::write(&nested, "-c constraints.txt\nchildpkg==2\n").expect("write nested");

        let parsed = parse_requirements_file(&main).expect("parse requirements");

        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].file, nested);
        assert_eq!(parsed.entries[0].line, 2);
        assert_eq!(parsed.entries[1].file, main);
        assert_eq!(parsed.constraints, vec![nested_dir.join("constraints.txt")]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_editable_local_directory_relative_to_file() {
        let root = temp_dir("editable");
        let package = root.join("pkg");
        fs::create_dir_all(&package).expect("create package dir");
        let file = root.join("requirements.txt");
        fs::write(&file, "-e ./pkg\n").expect("write requirements");

        let parsed = parse_requirements_file(&file).expect("parse requirements");

        assert_eq!(parsed.entries.len(), 1);
        let RequirementInput::Path { path, editable } = &parsed.entries[0].input else {
            panic!("expected editable path");
        };
        assert_eq!(path, &package);
        assert!(*editable);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_editable_urls_and_files() {
        let root = temp_dir("editable-reject");
        let file = root.join("requirements.txt");
        fs::write(&file, "-e https://example.test/demo.git\n").expect("write requirements");

        let error = parse_requirements_file(&file).expect_err("editable URL should fail");

        assert_eq!(error.to_string(), "editable requirements must be local directories");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolves_local_archive_requirements_relative_to_file() {
        let root = temp_dir("archive");
        let archive = root.join("demo-1.0.tar.gz");
        let file = root.join("requirements.txt");
        fs::write(&archive, b"").expect("write archive");
        fs::write(&file, "demo-1.0.tar.gz\n").expect("write requirements");

        let parsed = parse_requirements_file(&file).expect("parse requirements");

        let RequirementInput::Path { path, editable } = &parsed.entries[0].input else {
            panic!("expected archive path");
        };
        assert_eq!(path, &archive);
        assert!(!editable);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_unknown_options_with_file_and_line() {
        let root = temp_dir("unknown-option");
        let file = root.join("requirements.txt");
        fs::write(&file, "demo==1 --only-binary=:all:\n").expect("write requirements");

        let error = parse_requirements_file(&file).expect_err("unknown option should fail");

        assert_eq!(
            error.to_string(),
            format!("unsupported requirements option `--only-binary=:all:` in {}:1", file.display())
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn detects_circular_recursive_includes() {
        let root = temp_dir("cycle");
        let first = root.join("first.txt");
        let second = root.join("second.txt");
        fs::write(&first, "-r second.txt\n").expect("write first");
        fs::write(&second, "-r first.txt\n").expect("write second");

        let error = parse_requirements_file(&first).expect_err("cycle should fail");

        assert_eq!(
            error.to_string(),
            format!("circular -r include: {}", first.display())
        );
        fs::remove_dir_all(root).ok();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pon-requirements-{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn unique_env_key(label: &str) -> String {
        format!("PON_PM_REQUIREMENTS_{label}_{}_{}", std::process::id(), unique_suffix())
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    }

    struct EnvGuard {
        key: String,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: the test uses a process-unique key and restores it in Drop;
            // no production code relies on this test-only variable.
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_owned(),
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: this restores the process-unique test key changed by EnvGuard::set.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(&self.key, previous);
                } else {
                    std::env::remove_var(&self.key);
                }
            }
        }
    }
}
