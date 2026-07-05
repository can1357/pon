//! Native modules that are only exposed through package-manager installation.
//!
//! Pure-Python wheels are imported from `site-packages` by the generic source
//! loader.  This file is only for installed pon-native fixtures.

use std::{
	env, fs,
	path::{Path, PathBuf},
};

use super::install_module;
use crate::{abi::pon_const_str, intern::intern, object::PyObject};

const REGISTRY_FILE: &str = "native-modules.json";
const REGISTRY_ENV_VARS: &[&str] = &[
	"PON_NATIVE_MODULE_REGISTRY",
	"PON_NATIVE_MODULES_REGISTRY",
	"PON_PACKAGE_REGISTRY",
	"PON_REGISTRY",
];
const IMPORT_PATH_ENV_VARS: &[&str] = &["PON_IMPORT_PATH", "PONPATH"];

pub(super) fn make_module(name: &str) -> Result<Option<*mut PyObject>, String> {
	if name != "fastjson" || !is_installed(name) {
		return Ok(None);
	}

	make_fastjson().map(Some)
}
fn make_fastjson() -> Result<*mut PyObject, String> {
	let version = package_version("fastjson").unwrap_or_else(|| "0.1.0".to_owned());
	install_module("fastjson", vec![
		string_attr("__name__", "fastjson")?,
		string_attr("VERSION", &version)?,
	])
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate native package attribute {name}"))
}
fn is_installed(name: &str) -> bool {
	registry_texts()
		.iter()
		.any(|text| registry_mentions_module(text, name))
		|| import_roots()
			.iter()
			.any(|root| root_contains_module(root, name))
}

fn package_version(name: &str) -> Option<String> {
	for text in registry_texts() {
		if !registry_mentions_module(&text, name) {
			continue;
		}
		if let Some(version) = extract_version_near_module(&text, name) {
			return Some(version);
		}
		if name == "fastjson" {
			if let Some(version) = extract_string_key(&text, "VERSION") {
				return Some(version);
			}
		}
	}

	import_roots()
		.iter()
		.find_map(|root| version_from_import_root(root, name))
}

fn registry_texts() -> Vec<String> {
	let mut out = Vec::new();
	for var in REGISTRY_ENV_VARS {
		let Ok(value) = env::var(var) else {
			continue;
		};
		if value.trim().is_empty() {
			continue;
		}
		let path = PathBuf::from(&value);
		if path.is_file() {
			if let Ok(text) = fs::read_to_string(path) {
				out.push(text);
			}
		} else {
			out.push(value);
		}
	}

	for path in registry_paths() {
		if let Ok(text) = fs::read_to_string(path) {
			out.push(text);
		}
	}
	out
}

fn registry_paths() -> Vec<PathBuf> {
	let mut paths = Vec::new();
	if let Ok(home) = env::var("PON_HOME") {
		paths.push(PathBuf::from(home).join(REGISTRY_FILE));
	}
	if let Ok(cwd) = env::current_dir() {
		paths.push(cwd.join(".pon").join(REGISTRY_FILE));
	}
	for root in import_roots() {
		for ancestor in root.ancestors() {
			if ancestor.file_name().and_then(|name| name.to_str()) == Some(".pon") {
				paths.push(ancestor.join(REGISTRY_FILE));
				break;
			}
		}
	}
	paths
}

fn import_roots() -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for var in IMPORT_PATH_ENV_VARS {
		if let Ok(value) = env::var(var) {
			roots.extend(env::split_paths(&value));
		}
	}
	if let Ok(cwd) = env::current_dir() {
		roots.push(cwd.join(".pon").join("packages").join("site-packages"));
	}
	roots
}

fn registry_mentions_module(text: &str, name: &str) -> bool {
	let normalized = normalized_package_name(name);
	quoted_contains(text, name)
		|| quoted_contains(text, &normalized)
		|| text.contains(name)
		|| text.contains(&normalized)
}

fn quoted_contains(text: &str, needle: &str) -> bool {
	text.contains(&format!("\"{needle}\"")) || text.contains(&format!("'{needle}'"))
}

fn root_contains_module(root: &Path, name: &str) -> bool {
	if root.join(name).is_dir() || root.join(format!("{name}.py")).is_file() {
		return true;
	}
	if let Ok(entries) = fs::read_dir(root) {
		let normalized = normalized_package_name(name);
		for entry in entries.flatten() {
			let file_name = entry.file_name();
			let Some(file_name) = file_name.to_str() else {
				continue;
			};
			let lowered = file_name.to_ascii_lowercase();
			if lowered.starts_with(&normalized) || lowered.starts_with(name) {
				return true;
			}
		}
	}
	false
}

fn version_from_import_root(root: &Path, name: &str) -> Option<String> {
	let normalized = normalized_package_name(name);
	let entries = fs::read_dir(root).ok()?;
	for entry in entries.flatten() {
		let file_name = entry.file_name();
		let file_name = file_name.to_str()?;
		let lowered = file_name.to_ascii_lowercase();
		if !lowered.starts_with(&normalized) && !lowered.starts_with(name) {
			continue;
		}
		let path = entry.path();
		if path.is_dir() {
			for metadata_name in ["METADATA", "PKG-INFO", "pyproject.toml"] {
				let metadata_path = path.join(metadata_name);
				if let Ok(text) = fs::read_to_string(metadata_path) {
					if let Some(version) = extract_metadata_version(&text) {
						return Some(version);
					}
				}
			}
		}
	}

	let module_file = root.join(format!("{name}.py"));
	fs::read_to_string(module_file).ok().and_then(|text| {
		extract_assignment_string(&text, "VERSION")
			.or_else(|| extract_assignment_string(&text, "__version__"))
	})
}

fn extract_version_near_module(text: &str, name: &str) -> Option<String> {
	let normalized = normalized_package_name(name);
	for needle in [name, normalized.as_str()] {
		let Some(index) = text.find(needle) else {
			continue;
		};
		let end = text.len().min(index + 512);
		let window = &text[index..end];
		if let Some(version) =
			extract_string_key(window, "VERSION").or_else(|| extract_string_key(window, "version"))
		{
			return Some(version);
		}
	}
	None
}

fn extract_string_key(text: &str, key: &str) -> Option<String> {
	for quoted_key in [format!("\"{key}\""), format!("'{key}'"), key.to_owned()] {
		let Some(index) = text.find(&quoted_key) else {
			continue;
		};
		let after_key = &text[index + quoted_key.len()..];
		let after_sep = after_key
			.trim_start()
			.strip_prefix(':')
			.or_else(|| after_key.trim_start().strip_prefix('='))?;
		if let Some(value) = parse_quoted_value(after_sep.trim_start()) {
			return Some(value);
		}
	}
	None
}

fn extract_assignment_string(text: &str, key: &str) -> Option<String> {
	text.lines().find_map(|line| {
		let line = line.trim();
		let rhs = line
			.strip_prefix(key)?
			.trim_start()
			.strip_prefix('=')?
			.trim_start();
		parse_quoted_value(rhs)
	})
}

fn extract_metadata_version(text: &str) -> Option<String> {
	text.lines().find_map(|line| {
		let line = line.trim();
		if let Some(value) = line.strip_prefix("Version:") {
			return Some(value.trim().to_owned());
		}
		if let Some(value) = line.strip_prefix("version") {
			let value = value.trim_start().strip_prefix('=')?.trim();
			return parse_quoted_value(value).or_else(|| Some(value.to_owned()));
		}
		None
	})
}

fn parse_quoted_value(text: &str) -> Option<String> {
	let quote = text.as_bytes().first().copied()?;
	if quote != b'\'' && quote != b'\"' {
		return None;
	}
	let rest = &text[1..];
	let end = rest.find(char::from(quote))?;
	Some(rest[..end].to_owned())
}

fn normalized_package_name(name: &str) -> String {
	name.replace('_', "-").to_ascii_lowercase()
}
