//! System-linker invocation for AoT executables.

use std::{
	path::{Path, PathBuf},
	process::Command,
};

use anyhow::{Context, bail};
use target_lexicon::{BinaryFormat, Triple};

const RUNTIME_ARCHIVE: &str = "libpon_runtime.a";

/// Link Cranelift object files with the Pon runtime static archive.
pub fn link_executable(
	objects: &[PathBuf],
	runtime_a: &Path,
	out: &Path,
	triple: &Triple,
) -> anyhow::Result<()> {
	if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
		std::fs::create_dir_all(parent)
			.with_context(|| format!("failed to create {}", parent.display()))?;
	}
	let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
	let mut command = Command::new(&cc);
	command.args(objects).arg(runtime_a).arg("-o").arg(out);

	if triple.binary_format == BinaryFormat::Elf {
		command.args(["-lpthread", "-ldl", "-lm", "-lpanel", "-lncurses", "-llzma"]);
	} else if triple.binary_format == BinaryFormat::Macho {
		command.args([
			"-lpanel",
			"-lncurses",
			"-liconv",
			"-llzma",
			"-framework",
			"SystemConfiguration",
			"-framework",
			"CoreFoundation",
		]);
	}

	let rendered = render_command(&cc, objects, runtime_a, out, triple);
	let output = command
		.output()
		.with_context(|| format!("failed to invoke linker: {rendered}"))?;
	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);
		let stdout = String::from_utf8_lossy(&output.stdout);
		bail!(
			"linker failed with status {}\ncommand: {rendered}\nstdout:\n{stdout}\nstderr:\n{stderr}",
			output.status
		);
	}

	Ok(())
}

/// Locate `libpon_runtime.a` for AoT links.
pub fn locate_runtime_archive() -> anyhow::Result<PathBuf> {
	let mut tried = Vec::new();

	if let Some(path) = env_path("PON_RUNTIME_LIB") {
		tried.push(path.clone());
		if path.is_file() {
			return Ok(path);
		}
	}

	if let Ok(exe) = std::env::current_exe() {
		if let Some(exe_dir) = exe.parent() {
			let installed = exe_dir.join("..").join("lib").join(RUNTIME_ARCHIVE);
			tried.push(installed.clone());
			if installed.is_file() {
				return Ok(installed);
			}

			let side_by_side = exe_dir.join(RUNTIME_ARCHIVE);
			tried.push(side_by_side.clone());
			if side_by_side.is_file() {
				return Ok(side_by_side);
			}
		}
	}

	let target_dir = env_path("CARGO_TARGET_DIR").unwrap_or_else(|| PathBuf::from("target"));
	let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_owned());
	for candidate in [
		target_dir.join(&profile).join(RUNTIME_ARCHIVE),
		target_dir.join("debug").join(RUNTIME_ARCHIVE),
		target_dir.join("release").join(RUNTIME_ARCHIVE),
	] {
		tried.push(candidate.clone());
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	let tried = tried
		.iter()
		.map(|path| format!("  - {}", path.display()))
		.collect::<Vec<_>>()
		.join("\n");
	bail!("could not locate {RUNTIME_ARCHIVE}; tried:\n{tried}")
}

fn env_path(name: &str) -> Option<PathBuf> {
	std::env::var_os(name).map(PathBuf::from)
}

fn render_command(
	cc: &str,
	objects: &[PathBuf],
	runtime_a: &Path,
	out: &Path,
	triple: &Triple,
) -> String {
	let mut parts = vec![cc.to_owned()];
	parts.extend(objects.iter().map(|obj| obj.display().to_string()));
	parts.extend([runtime_a.display().to_string(), "-o".to_owned(), out.display().to_string()]);
	if triple.binary_format == BinaryFormat::Elf {
		parts.extend([
			"-lpthread".to_owned(),
			"-ldl".to_owned(),
			"-lm".to_owned(),
			"-lpanel".to_owned(),
			"-lncurses".to_owned(),
			"-llzma".to_owned(),
		]);
	} else if triple.binary_format == BinaryFormat::Macho {
		parts.extend([
			"-lpanel".to_owned(),
			"-lncurses".to_owned(),
			"-liconv".to_owned(),
			"-llzma".to_owned(),
			"-framework".to_owned(),
			"SystemConfiguration".to_owned(),
			"-framework".to_owned(),
			"CoreFoundation".to_owned(),
		]);
	}
	parts.join(" ")
}
