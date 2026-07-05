use pep508_rs::{MarkerEnvironment, MarkerEnvironmentBuilder};

/// pon's PEP 508 marker environment (F-U9): implementation_name="pon",
/// platform_python_implementation="Pon", Python 3.14, and host platform fields.
#[must_use]
pub fn pon_marker_env() -> MarkerEnvironment {
	MarkerEnvironment::try_from(MarkerEnvironmentBuilder {
		implementation_name: "pon",
		implementation_version: "3.14.0",
		os_name: os_name(),
		platform_machine: platform_machine(),
		platform_python_implementation: "Pon",
		platform_release: "",
		platform_system: platform_system(),
		platform_version: "",
		python_full_version: "3.14.0",
		python_version: "3.14",
		sys_platform: sys_platform(),
	})
	.expect("pon marker environment literals are valid PEP 440 versions")
}

fn os_name() -> &'static str {
	if cfg!(windows) { "nt" } else { "posix" }
}

fn sys_platform() -> &'static str {
	match std::env::consts::OS {
		"macos" => "darwin",
		"linux" => "linux",
		"windows" => "win32",
		other => other,
	}
}

fn platform_machine() -> &'static str {
	match (std::env::consts::OS, std::env::consts::ARCH) {
		("macos", "aarch64") => "arm64",
		(_, arch) => arch,
	}
}

fn platform_system() -> &'static str {
	match std::env::consts::OS {
		"macos" => "Darwin",
		"linux" => "Linux",
		"windows" => "Windows",
		other => other,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn marker_environment_reports_pon_python_314() {
		let env = pon_marker_env();

		assert_eq!(env.implementation_name(), "pon");
		assert_eq!(env.implementation_version().to_string(), "3.14.0");
		assert_eq!(env.python_full_version().to_string(), "3.14.0");
		assert_eq!(env.python_version().to_string(), "3.14");
		assert_eq!(env.platform_python_implementation(), "Pon");
		assert_eq!(env.platform_release(), "");
		assert_eq!(env.platform_version(), "");
	}
}
