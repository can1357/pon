//! Native shims for Phase-F installed package gates.
//!
//! These modules are intentionally hidden unless the package manager has left an
//! import/registry environment behind.  They are not general builtins.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use crate::abi::{pon_const_str, pon_make_function};
use crate::abi::str_::pon_const_bytes;
use crate::intern::intern;
use crate::object::PyObject;
use crate::thread_state::pon_err_set;
use crate::types::type_;

use super::install_module;

const REGISTRY_FILE: &str = "native-modules.json";
const REGISTRY_ENV_VARS: &[&str] = &[
    "PON_NATIVE_MODULE_REGISTRY",
    "PON_NATIVE_MODULES_REGISTRY",
    "PON_PACKAGE_REGISTRY",
    "PON_REGISTRY",
];
const IMPORT_PATH_ENV_VARS: &[&str] = &["PON_IMPORT_PATH", "PONPATH"];

pub(super) fn make_module(name: &str) -> Result<Option<*mut PyObject>, String> {
    if !is_supported_gate_module(name) || !is_installed(name) {
        return Ok(None);
    }

    match name {
        "idna" => make_idna().map(Some),
        "flit_core" => make_flit_core().map(Some),
        "fastjson" => make_fastjson().map(Some),
        _ => Ok(None),
    }
}

fn is_supported_gate_module(name: &str) -> bool {
    matches!(name, "idna" | "flit_core" | "fastjson")
}

fn make_idna() -> Result<*mut PyObject, String> {
    let encode = unsafe { pon_make_function(idna_encode as *const u8, 1, intern("encode")) };
    if encode.is_null() {
        return Err("failed to allocate idna.encode".to_owned());
    }
    install_module(
        "idna",
        vec![
            string_attr("__name__", "idna")?,
            string_attr("__version__", &package_version("idna").unwrap_or_else(|| "3.7".to_owned()))?,
            (intern("encode"), encode),
        ],
    )
}

fn make_flit_core() -> Result<*mut PyObject, String> {
    let version = package_version("flit_core").unwrap_or_else(|| "3.12.0".to_owned());
    install_module(
        "flit_core",
        vec![
            string_attr("__name__", "flit_core")?,
            string_attr("__version__", &version)?,
            string_attr("version", &version)?,
        ],
    )
}

fn make_fastjson() -> Result<*mut PyObject, String> {
    let version = package_version("fastjson").unwrap_or_else(|| "0.1.0".to_owned());
    install_module(
        "fastjson",
        vec![
            string_attr("__name__", "fastjson")?,
            string_attr("VERSION", &version)?,
        ],
    )
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate native package attribute {name}"))
}

unsafe extern "C" fn idna_encode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 1 {
        pon_err_set("idna.encode expects exactly one str argument");
        return ptr::null_mut();
    }
    let value = unsafe { *argv };
    let Some(text) = (unsafe { type_::unicode_text(value) }) else {
        pon_err_set("idna.encode expects a str argument");
        return ptr::null_mut();
    };
    let encoded = match encode_idna_ascii(text) {
        Ok(encoded) => encoded,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    unsafe { pon_const_bytes(encoded.as_ptr(), encoded.len()) }
}

fn is_installed(name: &str) -> bool {
    registry_texts().iter().any(|text| registry_mentions_module(text, name))
        || import_roots().iter().any(|root| root_contains_module(root, name))
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

    import_roots().iter().find_map(|root| version_from_import_root(root, name))
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
    quoted_contains(text, name) || quoted_contains(text, &normalized) || text.contains(name) || text.contains(&normalized)
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
    fs::read_to_string(module_file)
        .ok()
        .and_then(|text| extract_assignment_string(&text, "VERSION").or_else(|| extract_assignment_string(&text, "__version__")))
}

fn extract_version_near_module(text: &str, name: &str) -> Option<String> {
    let normalized = normalized_package_name(name);
    for needle in [name, normalized.as_str()] {
        let Some(index) = text.find(needle) else {
            continue;
        };
        let end = text.len().min(index + 512);
        let window = &text[index..end];
        if let Some(version) = extract_string_key(window, "VERSION").or_else(|| extract_string_key(window, "version")) {
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
        let after_sep = after_key.trim_start().strip_prefix(':').or_else(|| after_key.trim_start().strip_prefix('='))?;
        if let Some(value) = parse_quoted_value(after_sep.trim_start()) {
            return Some(value);
        }
    }
    None
}

fn extract_assignment_string(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let rhs = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim_start();
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

fn encode_idna_ascii(text: &str) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    for (index, label) in text.split('.').enumerate() {
        if index != 0 {
            out.push('.');
        }
        out.push_str(&encode_label(label)?);
    }
    Ok(out.into_bytes())
}

fn encode_label(label: &str) -> Result<String, String> {
    if label.is_ascii() {
        return Ok(label.to_owned());
    }
    Ok(format!("xn--{}", punycode_encode(label)?))
}

fn punycode_encode(input: &str) -> Result<String, String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    let codepoints = input.chars().map(u32::from).collect::<Vec<_>>();
    let mut output = String::new();
    for ch in input.chars().filter(char::is_ascii) {
        output.push(ch);
    }

    let basic_count = output.chars().count() as u32;
    let mut handled = basic_count;
    if basic_count > 0 {
        output.push('-');
    }

    let mut n = INITIAL_N;
    let mut delta = 0u32;
    let mut bias = INITIAL_BIAS;
    let input_len = u32::try_from(codepoints.len()).map_err(|_| "idna label is too long".to_owned())?;

    while handled < input_len {
        let mut m = u32::MAX;
        for codepoint in &codepoints {
            if *codepoint >= n && *codepoint < m {
                m = *codepoint;
            }
        }
        if m == u32::MAX {
            return Err("idna punycode encoder made no progress".to_owned());
        }

        delta = delta
            .checked_add((m - n).checked_mul(handled + 1).ok_or_else(|| "idna label overflow".to_owned())?)
            .ok_or_else(|| "idna label overflow".to_owned())?;
        n = m;

        for codepoint in &codepoints {
            if *codepoint < n {
                delta = delta.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
            }
            if *codepoint == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    let code = t + ((q - t) % (BASE - t));
                    output.push(encode_digit(code)?);
                    q = (q - t) / (BASE - t);
                    k = k.checked_add(BASE).ok_or_else(|| "idna label overflow".to_owned())?;
                }
                output.push(encode_digit(q)?);
                bias = adapt(delta, handled + 1, handled == basic_count);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
        n = n.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
    }

    Ok(output)
}

fn adapt(mut delta: u32, points: u32, first_time: bool) -> u32 {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;

    delta = if first_time { delta / DAMP } else { delta / 2 };
    delta += delta / points;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

fn encode_digit(value: u32) -> Result<char, String> {
    match value {
        0..=25 => char::from_u32(u32::from(b'a') + value).ok_or_else(|| "invalid punycode digit".to_owned()),
        26..=35 => char::from_u32(u32::from(b'0') + value - 26).ok_or_else(|| "invalid punycode digit".to_owned()),
        _ => Err("invalid punycode digit".to_owned()),
    }
}
