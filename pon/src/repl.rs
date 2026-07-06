use std::{
	borrow::Cow,
	env,
	io::{self, BufRead, IsTerminal, Write},
	path::PathBuf,
};

use anyhow::Result;
use pon_runtime::import::{begin_module_execution, end_module_execution, install_module};
use ruff_python_ast::PythonVersion;
use ruff_python_parser::{Mode, ParseOptions, TokenKind, parse, parse_unchecked};
use ruff_text_size::Ranged;
use rustyline::{
	Editor, Helper,
	completion::Completer,
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
	let mut editor = Editor::<ReplHelper, DefaultHistory>::new()?;
	editor.set_helper(Some(ReplHelper));
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

struct ReplHelper;

impl Helper for ReplHelper {}

impl Completer for ReplHelper {
	type Candidate = String;
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
	if buffer.is_empty() {
		if line.trim().is_empty() {
			return None;
		}
		if is_incomplete(&line) {
			buffer.push_str(&line);
			return None;
		}
		return Some(line);
	}

	if line.trim().is_empty() {
		return Some(std::mem::take(buffer));
	}
	buffer.push('\n');
	buffer.push_str(&line);
	None
}

fn is_incomplete(source: &str) -> bool {
	let options = ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY314);
	match parse(source, options) {
		Ok(_) => false,
		Err(error) => usize::from(error.location.end()) >= source.trim_end().len(),
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
