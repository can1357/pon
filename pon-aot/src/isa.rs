//! Cranelift ISA construction for ahead-of-time Pon objects.

use cranelift_codegen::{
	isa::{self, OwnedTargetIsa},
	settings::{self, Configurable, Flags},
};
use target_lexicon::{Architecture, BinaryFormat, Triple};

/// Build the tier-0 AoT ISA for `target`, or for the current host when omitted.
///
/// AoT always emits position-independent code, preserves frame pointers for the
/// conservative stack-walking contract, and enables inline stack probes on the
/// architectures where Cranelift supports the strategy used by the Phase C
/// plan.
#[must_use]
pub fn build_isa(target: Option<Triple>) -> OwnedTargetIsa {
	let triple = target.unwrap_or_else(Triple::host);
	let builder = if triple == Triple::host() {
		cranelift_native::builder_with_options(true)
			.expect("host architecture must be supported by Cranelift native builder")
	} else {
		isa::lookup(triple.clone()).expect("target architecture must be supported by Cranelift")
	};

	builder
		.finish(make_aot_flags(&triple))
		.expect("AoT Cranelift ISA must accept Pon flags")
}

/// Build the shared AoT Cranelift flags for `triple`.
#[must_use]
pub fn make_aot_flags(triple: &Triple) -> Flags {
	let mut builder = settings::builder();
	set(&mut builder, "opt_level", "none");
	set(&mut builder, "is_pic", "true");
	set(&mut builder, "preserve_frame_pointers", "true");
	set(&mut builder, "use_colocated_libcalls", "false");
	set(&mut builder, "tls_model", tls_model(triple));
	set(&mut builder, "enable_llvm_abi_extensions", "true");

	if supports_inline_probestack(triple) {
		set(&mut builder, "enable_probestack", "true");
		set(&mut builder, "probestack_strategy", "inline");
	} else {
		set(&mut builder, "enable_probestack", "false");
	}

	Flags::new(builder)
}

fn set(builder: &mut settings::Builder, key: &str, value: &str) {
	builder
		.set(key, value)
		.unwrap_or_else(|_| panic!("Cranelift 0.133.1 must support {key}={value}"));
}

fn tls_model(triple: &Triple) -> &'static str {
	match triple.binary_format {
		BinaryFormat::Macho => "macho",
		BinaryFormat::Coff => "coff",
		BinaryFormat::Elf => "elf_gd",
		_ => "none",
	}
}

fn supports_inline_probestack(triple: &Triple) -> bool {
	matches!(
		triple.architecture,
		Architecture::Aarch64(_)
			| Architecture::X86_64
			| Architecture::X86_64h
			| Architecture::Riscv64(_)
	)
}
