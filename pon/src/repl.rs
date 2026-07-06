use std::{
	borrow::Cow,
	cell::RefCell,
	collections::BTreeSet,
	env,
	io::{self, BufRead, IsTerminal, Write},
	path::PathBuf,
	rc::Rc,
};

use anyhow::Result;
use pon_runtime::import::{begin_module_execution, end_module_execution, install_module};
use ruff_python_ast::PythonVersion;
use ruff_python_parser::{Mode, ParseOptions, TokenKind, parse, parse_unchecked};
use ruff_text_size::Ranged;
use rustyline::{
	Context, Editor, Helper,
	completion::{Completer, Pair},
	error::ReadlineError,
	highlight::{CmdKind, Highlighter},
	hint::Hinter,
	history::DefaultHistory,
	validate::Validator,
};

/// Starts the interactive Pon session.
pub fn run() -> Result<()> {
	let argv = vec![String::new()];
	let mut stack_base_marker = 0usize;
	crate::run::boot_runtime(&argv, std::ptr::addr_of_mut!(stack_base_marker).cast::<u8>())?;
	install_module("__main__", Vec::<(u32, *mut pon_runtime::PyObject)>::new())
		.map_err(anyhow::Error::msg)?;
	begin_module_execution("__main__").map_err(anyhow::Error::msg)?;

	println!("pon {} (python 3.14)", env!("CARGO_PKG_VERSION"));
	if io::stdin().is_terminal() {
		run_terminal_loop()?;
	} else {
		run_piped_loop()?;
	}
	finish_session()
}

fn run_terminal_loop() -> Result<()> {
	// `ColorMode::Enabled` defers to rustyline's own terminal sniffing, which
	// goes dark under PTY harnesses (vhs recordings, expect). Stdout being a
	// live tty is the signal that matters for escape codes; force colors then
	// and stay automatic when stdout is redirected.
	let config = if io::stdout().is_terminal() {
		rustyline::Config::builder()
			.color_mode(rustyline::ColorMode::Forced)
			.build()
	} else {
		rustyline::Config::default()
	};
	let globals = Rc::new(RefCell::new(BTreeSet::new()));
	let mut editor = Editor::<ReplHelper, DefaultHistory>::with_config(config)?;
	editor.set_helper(Some(ReplHelper { globals: globals.clone() }));
	let history_path = env::var_os("HOME").map(|home| PathBuf::from(home).join(".pon_history"));
	if let Some(path) = history_path.as_deref() {
		let _ = editor.load_history(path);
	}

	let mut buffer = String::new();
	loop {
		let prompt = if buffer.is_empty() { ">>> " } else { "... " };
		match editor.readline(prompt) {
			Ok(line) => {
				if let Some(entry) = consume_line(&mut buffer, line) {
					let _ = editor.add_history_entry(entry.as_str());
					execute_entry(&entry)?;
					record_global_candidates(&globals, &entry);
				}
			},
			Err(ReadlineError::Interrupted) => {
				println!("KeyboardInterrupt");
				buffer.clear();
			},
			Err(ReadlineError::Eof) => {
				buffer.clear();
				break;
			},
			Err(error) => return Err(error.into()),
		}
	}

	if let Some(path) = history_path.as_deref() {
		let _ = editor.save_history(path);
	}
	Ok(())
}

struct ReplHelper {
	globals: Rc<RefCell<BTreeSet<String>>>,
}

impl Helper for ReplHelper {}

impl Completer for ReplHelper {
	type Candidate = Pair;

	fn complete(
		&self,
		line: &str,
		pos: usize,
		_ctx: &Context<'_>,
	) -> std::result::Result<(usize, Vec<Pair>), ReadlineError> {
		Ok(complete_python(&self.globals.borrow(), line, pos))
	}
}

impl Hinter for ReplHelper {
	type Hint = String;
}

impl Validator for ReplHelper {}

impl Highlighter for ReplHelper {
	fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
		highlight_python_syntax(line)
	}

	fn highlight_char(&self, _line: &str, _pos: usize, kind: CmdKind) -> bool {
		kind != CmdKind::MoveCursor
	}
}

const KEYWORD_COMPLETIONS: &[&str] = &[
	"False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
	"continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
	"if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
	"try", "while", "with", "yield",
];

const BUILTIN_COMPLETIONS: &[&str] = &[
	"abs", "all", "any", "ascii", "bin", "bool", "bytearray", "bytes", "callable", "chr",
	"classmethod", "complex", "dict", "dir", "divmod", "enumerate", "filter", "float",
	"format", "frozenset", "getattr", "globals", "hasattr", "hash", "hex", "id", "int",
	"isinstance", "issubclass", "iter", "len", "list", "locals", "map", "max", "min", "next",
	"object", "oct", "open", "ord", "pow", "print", "property", "range", "repr", "reversed",
	"round", "set", "setattr", "slice", "sorted", "staticmethod", "str", "sum", "super",
	"tuple", "type", "zip",
];

const STR_ATTR_COMPLETIONS: &[&str] = &[
	"capitalize", "casefold", "center", "count", "encode", "endswith", "find", "format",
	"format_map", "index", "isalnum", "isalpha", "isascii", "isdecimal", "isdigit",
	"isidentifier", "islower", "isnumeric", "isprintable", "isspace", "istitle", "isupper",
	"join", "ljust", "lower", "lstrip", "maketrans", "partition", "removeprefix",
	"removesuffix", "replace", "rfind", "rindex", "rjust", "rpartition", "rsplit", "rstrip",
	"split", "splitlines", "startswith", "strip", "swapcase", "title", "translate", "upper",
	"zfill",
];

const LIST_ATTR_COMPLETIONS: &[&str] = &[
	"append", "clear", "copy", "count", "extend", "index", "insert", "pop", "remove",
	"reverse", "sort",
];

fn complete_python(globals: &BTreeSet<String>, line: &str, pos: usize) -> (usize, Vec<Pair>) {
	let pos = pos.min(line.len());
	let start = completion_start(line, pos);
	let prefix = &line[start..pos];
	if let Some((receiver, attr_prefix)) = prefix.rsplit_once('.') {
		let attr_start = pos - attr_prefix.len();
		let attrs = attribute_candidates(receiver);
		return (attr_start, completion_pairs(attrs.into_iter(), attr_prefix));
	}

	let mut candidates = BTreeSet::new();
	candidates.extend(KEYWORD_COMPLETIONS.iter().copied());
	candidates.extend(BUILTIN_COMPLETIONS.iter().copied());
	for global in globals {
		candidates.insert(global.as_str());
	}
	(start, completion_pairs(candidates.into_iter(), prefix))
}

fn completion_start(line: &str, pos: usize) -> usize {
	line[..pos]
		.char_indices()
		.rev()
		.find(|(_, ch)| !((*ch == '_') || (*ch == '.') || ch.is_ascii_alphanumeric()))
		.map_or(0, |(index, ch)| index + ch.len_utf8())
}

fn completion_pairs<'a>(
	candidates: impl Iterator<Item = &'a str>,
	prefix: &str,
) -> Vec<Pair> {
	candidates
		.filter(|candidate| candidate.starts_with(prefix))
		.map(|candidate| Pair {
			display:     candidate.to_owned(),
			replacement: candidate.to_owned(),
		})
		.collect()
}

fn attribute_candidates(receiver: &str) -> BTreeSet<&'static str> {
	match receiver.rsplit('.').next().unwrap_or(receiver) {
		"str" => STR_ATTR_COMPLETIONS.iter().copied().collect(),
		"list" => LIST_ATTR_COMPLETIONS.iter().copied().collect(),
		"dict" => ["clear", "copy", "fromkeys", "get", "items", "keys", "pop", "popitem", "setdefault", "update", "values"].into_iter().collect(),
		"set" => ["add", "clear", "copy", "difference", "discard", "intersection", "pop", "remove", "union", "update"].into_iter().collect(),
		_ => BTreeSet::new(),
	}
}

fn record_global_candidates(globals: &Rc<RefCell<BTreeSet<String>>>, source: &str) {
	let mut globals = globals.borrow_mut();
	for line in source.lines().map(str::trim) {
		if let Some(rest) = line.strip_prefix("def ").or_else(|| line.strip_prefix("class ")) {
			if let Some(name) = leading_identifier(rest) {
				globals.insert(name.to_owned());
			}
		} else if let Some(rest) = line.strip_prefix("import ") {
			for part in rest.split(',') {
				let name = part
					.trim()
					.split_whitespace()
					.last()
					.unwrap_or("")
					.split('.')
					.next()
					.unwrap_or("");
				if is_identifier(name) {
					globals.insert(name.to_owned());
				}
			}
		} else if let Some((left, _)) = line.split_once('=') {
			for name in left.split(',').map(str::trim) {
				if is_identifier(name) {
					globals.insert(name.to_owned());
				}
			}
		}
	}
}

fn leading_identifier(input: &str) -> Option<&str> {
	let end = input
		.char_indices()
		.find(|(_, ch)| !((*ch == '_') || ch.is_ascii_alphanumeric()))
		.map_or(input.len(), |(index, _)| index);
	let name = &input[..end];
	is_identifier(name).then_some(name)
}

fn is_identifier(value: &str) -> bool {
	let mut chars = value.chars();
	matches!(chars.next(), Some('_') | Some('a'..='z') | Some('A'..='Z'))
		&& chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_KEYWORD: &str = "\x1b[1;34m";
const ANSI_LITERAL: &str = "\x1b[1;35m";
const ANSI_STRING: &str = "\x1b[32m";
const ANSI_NUMBER: &str = "\x1b[35m";
const ANSI_COMMENT: &str = "\x1b[90m";
const ANSI_OPERATOR: &str = "\x1b[90m";
const ANSI_ERROR: &str = "\x1b[31m";

fn highlight_python_syntax(source: &str) -> Cow<'_, str> {
	if source.is_empty() {
		return Cow::Borrowed(source);
	}

	let options = ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY314);
	let parsed = parse_unchecked(source, options);
	let mut output = None;
	let mut offset = 0usize;

	for token in parsed.tokens() {
		let range = token.range();
		let start = range.start().to_usize();
		let end = range.end().to_usize();
		if start >= end
			|| start < offset
			|| end > source.len()
			|| !source.is_char_boundary(start)
			|| !source.is_char_boundary(end)
		{
			continue;
		}

		let style = syntax_style(token.kind());
		if let Some(style) = style {
			let highlighted = output.get_or_insert_with(|| {
				let mut highlighted = String::with_capacity(source.len() + 32);
				highlighted.push_str(&source[..offset]);
				highlighted
			});
			highlighted.push_str(&source[offset..start]);
			highlighted.push_str(style);
			highlighted.push_str(&source[start..end]);
			highlighted.push_str(ANSI_RESET);
		} else if let Some(highlighted) = output.as_mut() {
			highlighted.push_str(&source[offset..end]);
		}
		offset = end;
	}

	if let Some(mut highlighted) = output {
		highlighted.push_str(&source[offset..]);
		Cow::Owned(highlighted)
	} else {
		Cow::Borrowed(source)
	}
}

fn syntax_style(kind: TokenKind) -> Option<&'static str> {
	match kind {
		TokenKind::False | TokenKind::None | TokenKind::True => Some(ANSI_LITERAL),
		kind if kind.is_keyword() => Some(ANSI_KEYWORD),
		TokenKind::String
		| TokenKind::FStringStart
		| TokenKind::FStringMiddle
		| TokenKind::FStringEnd
		| TokenKind::TStringStart
		| TokenKind::TStringMiddle
		| TokenKind::TStringEnd => Some(ANSI_STRING),
		TokenKind::Int | TokenKind::Float | TokenKind::Complex => Some(ANSI_NUMBER),
		TokenKind::Comment => Some(ANSI_COMMENT),
		TokenKind::Unknown => Some(ANSI_ERROR),
		kind if kind.is_operator() => Some(ANSI_OPERATOR),
		_ => None,
	}
}

fn run_piped_loop() -> Result<()> {
	let stdin = io::stdin();
	let mut buffer = String::new();
	for line in stdin.lock().lines() {
		if let Some(entry) = consume_line(&mut buffer, line?) {
			execute_entry(&entry)?;
		}
	}
	Ok(())
}

fn consume_line(buffer: &mut String, line: String) -> Option<String> {
	if buffer.is_empty() && line.trim().is_empty() {
		return None;
	}
	if !buffer.is_empty() {
		buffer.push('\n');
	}
	buffer.push_str(&line);
	if is_incomplete(buffer) {
		None
	} else {
		Some(std::mem::take(buffer))
	}
}

fn is_incomplete(source: &str) -> bool {
	let options = ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY314);
	match parse(source, options) {
		Ok(_) => false,
		Err(error) => error.location.end().to_usize() >= source.trim_end().len(),
	}
}

fn execute_entry(entry: &str) -> Result<()> {
	let result = crate::run::exec_interactive(entry);
	if let Some(code) = pon_runtime::abi::take_pending_system_exit() {
		io::stdout().flush().map_err(anyhow::Error::from)?;
		pon_runtime::native::atexit::run_exit_callbacks();
		end_module_execution("__main__");
		return Err(crate::run::SystemExitRequested(code).into());
	}

	match result {
		Ok(()) => Ok(()),
		Err(message) if pon_runtime::pon_err_occurred() => {
			unsafe {
				pon_runtime::pon_err_report_uncaught();
			}
			pon_runtime::thread_state::pon_err_clear();
			let _ = message;
			Ok(())
		},
		Err(message) => {
			eprintln!("{message}");
			Ok(())
		},
	}
}

fn finish_session() -> Result<()> {
	pon_runtime::native::atexit::run_exit_callbacks();
	end_module_execution("__main__");
	io::stdout().flush().map_err(anyhow::Error::from)
}
