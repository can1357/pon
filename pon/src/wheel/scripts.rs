//! Entry-point script parsing for wheel installation.

use crate::error::{Error, Result};

/// A console or GUI script declared by wheel `entry_points.txt` metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryPoint {
	/// Executable script name to create in the environment scripts directory.
	pub name:   String,
	/// Python module imported by the generated wrapper.
	pub module: String,
	/// Attribute path invoked by the generated wrapper; dotted attributes are
	/// preserved.
	pub attr:   String,
}

/// Entry-point script groups consumed by wheel installation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EntryPoints {
	/// Scripts declared in the `[console_scripts]` section.
	pub console_scripts: Vec<EntryPoint>,
	/// Scripts declared in the `[gui_scripts]` section.
	pub gui_scripts:     Vec<EntryPoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Section {
	ConsoleScripts,
	GuiScripts,
	Other,
}

/// Parse the wheel `entry_points.txt` INI subset used for script generation.
///
/// Only `[console_scripts]` and `[gui_scripts]` entries are collected. Blank
/// lines and full-line comments beginning with `#` or `;` are ignored. Entries
/// must be written as `name = module:attr`; an optional extras suffix such as
/// ` [extra]` or a trailing `; comment` after the target is ignored.
pub fn parse_entry_points(text: &str, label: &str) -> Result<EntryPoints> {
	let mut entry_points = EntryPoints::default();
	let mut section = Section::Other;

	for (line_index, raw_line) in text.lines().enumerate() {
		let line_number = line_index + 1;
		let line = raw_line.trim();
		if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
			continue;
		}

		if line.starts_with('[') {
			section = parse_section(line, label, line_number)?;
			continue;
		}

		match section {
			Section::ConsoleScripts => {
				entry_points
					.console_scripts
					.push(parse_script(line, label, line_number)?)
			},
			Section::GuiScripts => {
				entry_points
					.gui_scripts
					.push(parse_script(line, label, line_number)?)
			},
			Section::Other => {},
		}
	}

	Ok(entry_points)
}

fn parse_section(line: &str, label: &str, line_number: usize) -> Result<Section> {
	let Some(close_index) = line.find(']') else {
		return Err(malformed(label, line_number, "section header is missing `]`"));
	};
	let section_name = line[1..close_index].trim();
	let trailing = line[close_index + 1..].trim();
	if !trailing.is_empty() && !trailing.starts_with('#') && !trailing.starts_with(';') {
		return Err(malformed(label, line_number, "section header has trailing content"));
	}

	Ok(match section_name {
		"console_scripts" => Section::ConsoleScripts,
		"gui_scripts" => Section::GuiScripts,
		_ => Section::Other,
	})
}

fn parse_script(line: &str, label: &str, line_number: usize) -> Result<EntryPoint> {
	let Some((name, target)) = line.split_once('=') else {
		return Err(malformed(label, line_number, "script entry is missing `=`"));
	};
	let name = name.trim();
	if name.is_empty() {
		return Err(malformed(label, line_number, "script name is empty"));
	}

	let target = strip_entry_point_suffix(target);
	let Some((module, attr)) = target.split_once(':') else {
		return Err(malformed(label, line_number, "script target is missing `:`"));
	};
	let module = module.trim();
	let attr = attr.trim();
	if module.is_empty() {
		return Err(malformed(label, line_number, "script target module is empty"));
	}
	if attr.is_empty() {
		return Err(malformed(label, line_number, "script target attribute is empty"));
	}

	Ok(EntryPoint { name: name.to_owned(), module: module.to_owned(), attr: attr.to_owned() })
}

fn strip_entry_point_suffix(target: &str) -> &str {
	let target = target
		.split_once(';')
		.map_or(target, |(before_comment, _)| before_comment)
		.trim();
	if let Some(open_index) = target.rfind('[') {
		if target.ends_with(']')
			&& target[..open_index]
				.chars()
				.next_back()
				.is_some_and(char::is_whitespace)
		{
			return target[..open_index].trim_end();
		}
	}
	target
}

fn malformed(label: &str, line_number: usize, reason: &str) -> Error {
	Error::UnsupportedArtifact(format!("entry points `{label}` line {line_number}: {reason}"))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(name: &str, module: &str, attr: &str) -> EntryPoint {
		EntryPoint { name: name.to_owned(), module: module.to_owned(), attr: attr.to_owned() }
	}

	#[test]
	fn parses_console_and_gui_scripts() {
		let parsed = parse_entry_points(
			"\
# generated metadata\n[console_scripts]\ndemo = demo.cli:main\n\n[gui_scripts]\ndemo-gui = \
			 demo.gui:start\n",
			"entry_points.txt",
		)
		.expect("entry points parse");

		assert_eq!(parsed.console_scripts, vec![entry("demo", "demo.cli", "main")]);
		assert_eq!(parsed.gui_scripts, vec![entry("demo-gui", "demo.gui", "start")]);
	}

	#[test]
	fn skips_comments_and_blank_lines() {
		let parsed = parse_entry_points(
			"\
[console_scripts]\n; comment\n\n# another comment\ndemo = demo.cli:main\n",
			"entry_points.txt",
		)
		.expect("entry points parse");

		assert_eq!(parsed.console_scripts, vec![entry("demo", "demo.cli", "main")]);
		assert!(parsed.gui_scripts.is_empty());
	}

	#[test]
	fn ignores_extras_suffixes() {
		let parsed = parse_entry_points(
			"\
[console_scripts]\ndemo = demo.cli:main [cli]\nlegacy = legacy.cli:main ; extra == 'cli'\n",
			"entry_points.txt",
		)
		.expect("entry points parse");

		assert_eq!(parsed.console_scripts, vec![
			entry("demo", "demo.cli", "main"),
			entry("legacy", "legacy.cli", "main")
		]);
	}

	#[test]
	fn preserves_dotted_attribute_paths() {
		let parsed = parse_entry_points(
			"\
[console_scripts]\ndemo = demo.cli:Command.run\n",
			"entry_points.txt",
		)
		.expect("entry points parse");

		assert_eq!(parsed.console_scripts, vec![entry("demo", "demo.cli", "Command.run")]);
	}
}
