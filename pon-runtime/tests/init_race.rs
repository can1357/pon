//! Regression test for the `init_runtime` initialization race fixed by the
//! `InitPhase` gate in pon-runtime/src/abi.rs.
//!
//! Pre-fix, `init_runtime` published the runtime singleton and released the
//! runtime mutex BEFORE eager native-module registration ran, so a concurrent
//! `pon_runtime_init()` caller saw the occupied slot, returned 0, and then hit
//! "sys module is not initialized" in `pon_sys_set_argv` (package-manager PEP 517
//! flake). Contract: init returning 0 on ANY thread implies the eager runtime
//! surface — in particular the cached `sys` module — is visible to that thread.

use std::ffi::CString;
use std::sync::{Arc, Barrier};
use std::thread;

use pon_runtime::import::cached_module;
use pon_runtime::{intern, pon_runtime_init, pon_sys_set_argv};

#[test]
fn concurrent_init_racers_see_fully_registered_runtime() {
    const RACERS: usize = 8;
    let barrier = Arc::new(Barrier::new(RACERS));

    let workers: Vec<_> = (0..RACERS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();

                let rc = unsafe { pon_runtime_init() };
                assert_eq!(rc, 0, "pon_runtime_init reported failure on a racer thread");

                // The exact read that raced pre-fix: init reported success
                // while `register_native_modules` had not yet cached `sys`.
                assert!(
                    cached_module(intern("sys")).is_some(),
                    "pon_runtime_init returned 0 but the sys module is not cached"
                );
            })
        })
        .collect();

    for worker in workers {
        worker.join().expect("init racer thread panicked");
    }

    // The real victim of the pre-fix window: setting sys.argv right after a
    // successful init. Done once, post-join, because concurrent argv installs
    // would race on the module attribute table itself, which is not the
    // contract under test.
    let script = CString::new("t.py").unwrap();
    let argv = [script.as_ptr().cast::<u8>()];
    let rc = unsafe { pon_sys_set_argv(1, argv.as_ptr()) };
    assert_eq!(rc, 0, "pon_sys_set_argv failed after successful concurrent init");
}
