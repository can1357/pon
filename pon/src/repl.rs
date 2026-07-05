use std::env;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::Result;
use pon_runtime::import::{begin_module_execution, end_module_execution, install_module};
use ruff_python_ast::PythonVersion;
use ruff_python_parser::{parse, Mode, ParseOptions};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

/// Starts the interactive Pon session.
pub fn run() -> Result<()> {
    let argv = vec![String::new()];
    let mut stack_base_marker = 0usize;
    crate::run::boot_runtime(&argv, std::ptr::addr_of_mut!(stack_base_marker).cast::<u8>())?;
    install_module("__main__", Vec::<(u32, *mut pon_runtime::PyObject)>::new()).map_err(anyhow::Error::msg)?;
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
    let mut editor = DefaultEditor::new()?;
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
            }
            Err(ReadlineError::Interrupted) => {
                println!("KeyboardInterrupt");
                buffer.clear();
            }
            Err(ReadlineError::Eof) => {
                buffer.clear();
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }

    if let Some(path) = history_path.as_deref() {
        let _ = editor.save_history(path);
    }
    Ok(())
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
        }
        Err(message) => {
            eprintln!("{message}");
            Ok(())
        }
    }
}

fn finish_session() -> Result<()> {
    pon_runtime::native::atexit::run_exit_callbacks();
    end_module_execution("__main__");
    io::stdout().flush().map_err(anyhow::Error::from)
}
