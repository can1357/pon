//! Process-boundary `sys` and stdio hooks shared by AoT entry code.

use std::ffi::{CStr, c_char};
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use crate::abi::{pon_const_str, return_minus_one_with_error};
use crate::import::{PyModuleObject, cached_module};
use crate::intern::intern;
use crate::object::PyObject;

/// Decodes process arguments and installs them as `sys.argv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_sys_set_argv(argc: i32, argv: *const *const u8) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| unsafe { set_argv_impl(argc, argv) })) {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => return_minus_one_with_error(message),
        Err(_) => return_minus_one_with_error("setting sys.argv panicked"),
    }
}

unsafe fn set_argv_impl(argc: i32, argv: *const *const u8) -> Result<(), String> {
    if argc < 0 {
        return Err("argc is negative".to_owned());
    }
    let argc = argc as usize;
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is NULL".to_owned());
    }

    let mut values = Vec::with_capacity(argc);
    for index in 0..argc {
        let raw = unsafe { *argv.add(index) };
        if raw.is_null() {
            return Err(format!("argv[{index}] is NULL"));
        }
        let text = unsafe { CStr::from_ptr(raw.cast::<c_char>()) }
            .to_str()
            .map_err(|_| format!("argv[{index}] is not valid UTF-8"))?;
        let object = unsafe { pon_const_str(text.as_ptr(), text.len()) };
        if object.is_null() {
            return Err(format!("failed to allocate sys.argv[{index}]"));
        }
        values.push(object);
    }

    let argv_object = if values.is_empty() {
        unsafe { crate::abi::seq::pon_build_list(ptr::null_mut(), 0) }
    } else {
        unsafe { crate::abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
    };
    if argv_object.is_null() {
        return Err("failed to allocate sys.argv".to_owned());
    }

    install_sys_argv(argv_object)
}

fn install_sys_argv(argv_object: *mut PyObject) -> Result<(), String> {
    let sys = cached_module(intern("sys")).ok_or_else(|| "sys module is not initialized".to_owned())?;
    let sys = sys.cast::<PyModuleObject>();
    unsafe {
        (&mut *sys).attrs.insert(intern("argv"), argv_object);
    }
    // J0.3 GlobalIC site: module attr overlay mutation.
    crate::abi::bump_namespace_version();
    Ok(())
}

/// Flushes process stdout and stderr before returning from an AoT executable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_io_flush_std() -> i32 {
    match catch_unwind(AssertUnwindSafe(flush_std_impl)) {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => return_minus_one_with_error(message),
        Err(_) => return_minus_one_with_error("stdio flush panicked"),
    }
}

fn flush_std_impl() -> Result<(), String> {
    io::stdout().flush().map_err(|error| error.to_string())?;
    io::stderr().flush().map_err(|error| error.to_string())?;
    Ok(())
}
