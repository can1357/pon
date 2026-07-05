//! Mach-O build-version metadata for Cranelift-emitted objects.

use cranelift_object::object::{self, BinaryFormat};
use target_lexicon::{OperatingSystem, Triple};

/// Stamp `LC_BUILD_VERSION` on Mach-O objects.
///
/// Cranelift-object may already set this for versioned Apple triples. This
/// helper deliberately overwrites it with Pon's Phase C floor so version-less
/// host triples still avoid Xcode's "no platform load command" warning.
pub fn stamp_macho_build_version(obj: &mut object::write::Object<'_>, triple: &Triple) {
	if obj.format() != BinaryFormat::MachO || !is_macos_like(triple) {
		return;
	}

	let mut build_version = object::write::MachOBuildVersion::default();
	build_version.platform = object::macho::PLATFORM_MACOS;
	build_version.minos = deployment_target_minos();
	build_version.sdk = 0;
	obj.set_macho_build_version(build_version);
}

fn is_macos_like(triple: &Triple) -> bool {
	matches!(triple.operating_system, OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_))
}

fn deployment_target_minos() -> u32 {
	std::env::var("MACOSX_DEPLOYMENT_TARGET")
		.ok()
		.and_then(|value| parse_version(&value))
		.unwrap_or_else(|| pack_version(11, 0, 0))
}

fn parse_version(value: &str) -> Option<u32> {
	let mut parts = value.split('.');
	let major = parts.next()?.parse::<u32>().ok()?;
	let minor = parts
		.next()
		.map_or(Some(0), |part| part.parse::<u32>().ok())?;
	let patch = parts
		.next()
		.map_or(Some(0), |part| part.parse::<u32>().ok())?;
	Some(pack_version(major, minor, patch))
}

fn pack_version(major: u32, minor: u32, patch: u32) -> u32 {
	(major << 16) | (minor << 8) | patch
}
