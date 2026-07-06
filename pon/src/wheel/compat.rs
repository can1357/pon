use std::{collections::BTreeSet, fmt, path::Path};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Tag {
	pub python:   String,
	pub abi:      String,
	pub platform: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WheelCompatibility {
	PurePython,
	CAbiRefused { reason: String },
}

impl Tag {
	#[must_use]
	pub fn new(
		python: impl Into<String>,
		abi: impl Into<String>,
		platform: impl Into<String>,
	) -> Self {
		Self { python: python.into(), abi: abi.into(), platform: platform.into() }
	}
}

impl fmt::Display for Tag {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}-{}-{}", self.python, self.abi, self.platform)
	}
}

#[must_use]
pub fn parse_tag_set(python: &str, abi: &str, platform: &str) -> Vec<Tag> {
	let mut tags = Vec::new();
	for py in python.split('.') {
		for abi in abi.split('.') {
			for platform in platform.split('.') {
				tags.push(Tag::new(py, abi, platform));
			}
		}
	}
	tags
}

const CURRENT_PYTHON_MINOR: u8 = 14;

fn supported_python_rank(python: &str) -> Option<u8> {
	if python == "py3" {
		return Some(0);
	}

	let minor = python.strip_prefix("py3")?;
	if minor.is_empty() || (minor.len() > 1 && minor.starts_with('0')) {
		return None;
	}
	let minor = minor.parse::<u8>().ok()?;
	if minor <= CURRENT_PYTHON_MINOR {
		Some(minor + 1)
	} else {
		None
	}
}

/// True when `tag` names a wheel pon built for its own ABI on this host:
/// interpreter `pon3<minor>`, ABI `pon`, and a platform tag for the current
/// OS/architecture (`any` accepted). These wheels come out of pon's own PEP
/// 517 builds — the CPython C-ABI refusal does not apply to them.
#[must_use]
pub fn pon_native_tag(tag: &Tag) -> bool {
	if tag.python != format!("pon3{CURRENT_PYTHON_MINOR}") || tag.abi != "pon" {
		return false;
	}
	platform_matches_host(&tag.platform)
}

fn platform_matches_host(platform: &str) -> bool {
	if platform == "any" {
		return true;
	}
	let os_matches = if cfg!(target_os = "macos") {
		platform.starts_with("macosx")
	} else if cfg!(target_os = "linux") {
		platform.contains("linux")
	} else if cfg!(windows) {
		platform.starts_with("win")
	} else {
		false
	};
	let arch_matches = if cfg!(target_arch = "aarch64") {
		platform.ends_with("arm64") || platform.ends_with("aarch64")
	} else if cfg!(target_arch = "x86_64") {
		platform.ends_with("x86_64") || platform.ends_with("amd64")
	} else {
		false
	};
	os_matches && arch_matches
}

#[must_use]
pub fn supported_tag_rank(tag: &Tag) -> Option<u8> {
	if pon_native_tag(tag) {
		// Outranks every pure-Python tag: a host-ABI build is the most
		// specific artifact pon can install.
		return Some(CURRENT_PYTHON_MINOR + 2);
	}
	if tag.abi == "none" && tag.platform == "any" {
		supported_python_rank(&tag.python)
	} else {
		None
	}
}

#[must_use]
pub fn best_supported_tag_rank(candidate: &[Tag]) -> Option<u8> {
	candidate.iter().filter_map(supported_tag_rank).max()
}

#[must_use]
pub fn default_supported_tags() -> BTreeSet<Tag> {
	let mut tags = BTreeSet::new();
	tags.insert(Tag::new("py3", "none", "any"));
	for minor in 0..=CURRENT_PYTHON_MINOR {
		tags.insert(Tag::new(format!("py3{minor}"), "none", "any"));
	}
	tags
}

#[must_use]
pub fn any_supported(candidate: &[Tag], supported: &BTreeSet<Tag>) -> bool {
	candidate.iter().any(|tag| {
		pon_native_tag(tag) || (supported.contains(tag) && supported_tag_rank(tag).is_some())
	})
}

#[must_use]
pub fn classify_tags(candidate: &[Tag], supported: &BTreeSet<Tag>) -> WheelCompatibility {
	if any_supported(candidate, supported) {
		WheelCompatibility::PurePython
	} else {
		let candidate_tags = candidate
			.iter()
			.map(ToString::to_string)
			.collect::<Vec<_>>()
			.join(", ");
		WheelCompatibility::CAbiRefused {
			reason: format!(
				"Pon can install pure Python py*-none-any wheels only; candidate tags \
				 `{candidate_tags}` target a C ABI or platform wheel"
			),
		}
	}
}

/// `Root-Is-Purelib` gate. `pon_native` wheels legitimately install into
/// platlib (`Root-Is-Purelib: false`); everything else must be purelib.
#[must_use]
pub fn classify_root_is_purelib(metadata: &str, pon_native: bool) -> WheelCompatibility {
	if pon_native {
		return WheelCompatibility::PurePython;
	}
	for line in metadata.lines() {
		let Some((key, value)) = line.split_once(':') else {
			continue;
		};
		if key.trim().eq_ignore_ascii_case("Root-Is-Purelib") {
			return if value.trim().eq_ignore_ascii_case("true") {
				WheelCompatibility::PurePython
			} else {
				WheelCompatibility::CAbiRefused {
					reason: "wheel metadata Root-Is-Purelib is not true".to_owned(),
				}
			};
		}
	}
	WheelCompatibility::CAbiRefused { reason: "wheel metadata omits Root-Is-Purelib".to_owned() }
}

/// Native-extension member gate. Pon's own `.pon.so` extensions are the one
/// allowed native shape; CPython `.so`/`.pyd`/`.dylib` members stay refused.
#[must_use]
pub fn classify_archive_member(path: &str) -> WheelCompatibility {
	if path.ends_with(".pon.so") {
		return WheelCompatibility::PurePython;
	}
	let extension = Path::new(path)
		.extension()
		.and_then(|extension| extension.to_str())
		.map(str::to_ascii_lowercase);
	match extension.as_deref() {
		Some("so" | "pyd" | "dylib") => WheelCompatibility::CAbiRefused {
			reason: format!("wheel archive contains native extension member `{path}`"),
		},
		_ => WheelCompatibility::PurePython,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn expands_compressed_tag_sets() {
		let tags = parse_tag_set("py2.py3", "none", "any");
		assert_eq!(tags, vec![Tag::new("py2", "none", "any"), Tag::new("py3", "none", "any")]);
	}

	#[test]
	fn detects_supported_pure_python_tags() {
		let supported = default_supported_tags();
		for python in ["py3", "py30", "py39", "py310", "py314"] {
			let tags = parse_tag_set(python, "none", "any");
			assert!(any_supported(&tags, &supported), "{python}-none-any should be supported");
		}

		let py2_only = parse_tag_set("py2", "none", "any");
		assert!(!supported.contains(&Tag::new("py2", "none", "any")));
		assert!(!any_supported(&py2_only, &supported));

		let py2_py3 = parse_tag_set("py2.py3", "none", "any");
		assert!(any_supported(&py2_py3, &supported));

		for tags in [
			parse_tag_set("py315", "none", "any"),
			parse_tag_set("cp312", "abi3", "macosx_14_0_arm64"),
			parse_tag_set("py314", "cp314", "any"),
			parse_tag_set("py314", "none", "macosx_14_0_arm64"),
		] {
			assert!(!any_supported(&tags, &supported));
		}
	}

	#[test]
	fn ranks_supported_python_tags_by_specificity() {
		let py3 = supported_tag_rank(&Tag::new("py3", "none", "any")).unwrap();
		let py312 = supported_tag_rank(&Tag::new("py312", "none", "any")).unwrap();
		let py313 = supported_tag_rank(&Tag::new("py313", "none", "any")).unwrap();
		let py314 = supported_tag_rank(&Tag::new("py314", "none", "any")).unwrap();

		assert!(py314 > py313);
		assert!(py313 > py312);
		assert!(py312 > py3);
		assert_eq!(
			best_supported_tag_rank(&parse_tag_set("py3.py312.py314", "none", "any")),
			Some(py314)
		);
		assert_eq!(best_supported_tag_rank(&parse_tag_set("py2", "none", "any")), None);
	}

	#[test]
	fn classifies_root_is_purelib_metadata() {
		assert_eq!(
			classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: true\n", false),
			WheelCompatibility::PurePython
		);
		assert!(matches!(
			classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: false\n", false),
			WheelCompatibility::CAbiRefused { .. }
		));
		// pon-native wheels install into platlib by design.
		assert_eq!(
			classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: false\n", true),
			WheelCompatibility::PurePython
		);
	}

	#[test]
	fn classifies_native_archive_members() {
		assert_eq!(classify_archive_member("pkg/__init__.py"), WheelCompatibility::PurePython);
		assert!(matches!(
			classify_archive_member("pkg/_speedups.cpython-314-darwin.so"),
			WheelCompatibility::CAbiRefused { .. }
		));
		// Pon's own extension suffix is the one allowed native member shape.
		assert_eq!(
			classify_archive_member("numpy/_core/_multiarray_umath.pon.so"),
			WheelCompatibility::PurePython
		);
	}

	#[test]
	fn accepts_pon_native_wheel_tags_for_this_host() {
		let host_platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
			"macosx_26_0_arm64"
		} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
			"linux_x86_64"
		} else {
			"any"
		};
		let native = Tag::new("pon314", "pon", host_platform);
		assert!(pon_native_tag(&native));
		assert!(any_supported(&[native.clone()], &default_supported_tags()));
		// Native host builds outrank every pure-Python tag.
		let py314 = supported_tag_rank(&Tag::new("py314", "none", "any")).unwrap();
		assert!(supported_tag_rank(&native).unwrap() > py314);
		// CPython ABI tags and foreign-platform pon tags stay refused.
		assert!(!pon_native_tag(&Tag::new("cp314", "cp314", host_platform)));
		assert!(!pon_native_tag(&Tag::new("pon314", "pon", "sunos5_sparc")));
		assert!(matches!(
			classify_tags(
				&parse_tag_set("cp314", "cp314", "macosx_11_0_arm64"),
				&default_supported_tags()
			),
			WheelCompatibility::CAbiRefused { .. }
		));
	}
}
