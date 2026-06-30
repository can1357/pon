//! Shared Cranelift ISA configuration for Phase A codegen consumers.
//!
//! Baseline lowering and helper-call emission live in later waves. This module
//! only centralizes the shared flags that every codegen frontend must use so the
//! JIT and AOT paths agree on stack walking, PIC mode, and libcall relocation
//! behavior.

use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable, Flags};

/// Optimization level exposed by `pon-codegen`.
///
/// This mirrors the Cranelift shared `opt_level` values without leaking
/// Cranelift's generated settings enum through our public API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptLevel {
    /// Disable optimization.
    None,
    /// Optimize for execution speed.
    Speed,
    /// Optimize for execution speed while considering code size.
    SpeedAndSize,
}

impl OptLevel {
    fn as_setting_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Speed => "speed",
            Self::SpeedAndSize => "speed_and_size",
        }
    }
}

/// Build the shared Cranelift flags used by all Phase A codegen entry points.
///
/// Frame pointers are always preserved for runtime/debuggability contracts.
/// Colocated libcalls are disabled so JIT/AArch64 call relocations can use the
/// long-range path expected by later helper lowering.
#[must_use]
pub fn make_shared_flags(opt: OptLevel, pic: bool) -> Flags {
    let mut builder = settings::builder();
    builder
        .set("opt_level", opt.as_setting_value())
        .expect("Cranelift 0.133.1 must support the shared opt_level setting");
    builder
        .set("is_pic", if pic { "true" } else { "false" })
        .expect("Cranelift 0.133.1 must support the shared is_pic setting");
    builder
        .set("preserve_frame_pointers", "true")
        .expect("Cranelift 0.133.1 must support preserve_frame_pointers");
    builder
        .set("use_colocated_libcalls", "false")
        .expect("Cranelift 0.133.1 must support use_colocated_libcalls");

    Flags::new(builder)
}

/// Build a native Cranelift ISA using Pon's shared Phase A flags.
///
/// The returned ISA targets the host architecture detected by
/// `cranelift_native::builder()` and is suitable for later baseline/JIT
/// consumers.
#[must_use]
pub fn make_isa(opt: OptLevel, pic: bool) -> OwnedTargetIsa {
    cranelift_native::builder()
        .expect("host architecture must be supported by Cranelift native builder")
        .finish(make_shared_flags(opt, pic))
        .expect("native Cranelift ISA must accept Pon shared flags")
}

#[cfg(test)]
mod tests {
    use cranelift_codegen::settings;

    use super::{make_shared_flags, OptLevel};

    #[test]
    fn shared_flags_disable_pic_at_no_optimization() {
        let flags = make_shared_flags(OptLevel::None, false);

        assert_eq!(flags.opt_level(), settings::OptLevel::None);
        assert!(!flags.is_pic());
        assert!(flags.preserve_frame_pointers());
        assert!(!flags.use_colocated_libcalls());
    }

    #[test]
    fn shared_flags_enable_pic_at_speed_optimization() {
        let flags = make_shared_flags(OptLevel::Speed, true);

        assert_eq!(flags.opt_level(), settings::OptLevel::Speed);
        assert!(flags.is_pic());
        assert!(flags.preserve_frame_pointers());
        assert!(!flags.use_colocated_libcalls());
    }
}
