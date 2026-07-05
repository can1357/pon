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

#[must_use]
pub fn supported_tag_rank(tag: &Tag) -> Option<u8> {
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
	candidate
		.iter()
		.any(|tag| supported.contains(tag) && supported_tag_rank(tag).is_some())
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

#[must_use]
pub fn classify_root_is_purelib(metadata: &str) -> WheelCompatibility {
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

#[must_use]
pub fn classify_archive_member(path: &str) -> WheelCompatibility {
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
			classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: true\n"),
			WheelCompatibility::PurePython
		);
		assert!(matches!(
			classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: false\n"),
			WheelCompatibility::CAbiRefused { .. }
		));
	}

	#[test]
	fn classifies_native_archive_members() {
		assert_eq!(classify_archive_member("pkg/__init__.py"), WheelCompatibility::PurePython);
		assert!(matches!(
			classify_archive_member("pkg/_speedups.cpython-314-darwin.so"),
			WheelCompatibility::CAbiRefused { .. }
		));
	}
}
