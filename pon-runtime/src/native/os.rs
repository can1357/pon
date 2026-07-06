//! Native `os` module seed for WS-IMPORT.

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{CodeInfo, ParamSpec, pon_const_str},
	intern::intern,
	object::PyObject,
	types::exc::ExceptionKind,
};
unsafe extern "C" {
	fn ctermid(s: *mut libc::c_char) -> *mut libc::c_char;
	#[cfg(target_os = "macos")]
	fn lchflags(path: *const libc::c_char, flags: libc::c_uint) -> libc::c_int;
	#[cfg(target_os = "macos")]
	fn lchmod(path: *const libc::c_char, mode: libc::mode_t) -> libc::c_int;
	fn fstatvfs(fd: libc::c_int, buf: *mut libc::statvfs) -> libc::c_int;
	fn statvfs(path: *const libc::c_char, buf: *mut libc::statvfs) -> libc::c_int;
	fn wait3(
		status: *mut libc::c_int,
		options: libc::c_int,
		rusage: *mut libc::rusage,
	) -> libc::pid_t;
	fn wait4(
		pid: libc::pid_t,
		status: *mut libc::c_int,
		options: libc::c_int,
		rusage: *mut libc::rusage,
	) -> libc::pid_t;
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("os", build_attrs("os")?)
}

/// Attr set shared by the curated `os` and `posix` modules.
///
/// On POSIX hosts CPython's `os.py` re-exports the C `posix` module wholesale
/// (`from posix import *`), so both names must serve one surface; `posix.rs`
/// installs this same set under the other name.
pub(super) fn build_attrs(module: &'static str) -> Result<Vec<(u32, *mut PyObject)>, String> {
	let sep = if cfg!(windows) { "\\" } else { "/" };
	let linesep = if cfg!(windows) { "\r\n" } else { "\n" };
	let attrs = [
		string_attr(module, "__name__", module),
		string_attr(module, "name", os_name()),
		string_attr(module, "sep", sep),
		string_attr(module, "pathsep", if cfg!(windows) { ";" } else { ":" }),
		string_attr(module, "linesep", linesep),
		string_attr(module, "curdir", "."),
		string_attr(module, "pardir", ".."),
	];
	let mut attrs = attrs.into_iter().collect::<Result<Vec<_>, _>>()?;
	if module == "os" {
		attrs.push((intern("altsep"), unsafe { crate::abi::pon_none() }));
	}
	for &(name, value) in [OPEN_FLAGS, ACCESS_FLAGS, WAIT_OPTIONS, SEEK_MODES, POSIX_CONSTANTS]
		.into_iter()
		.flatten()
	{
		// SAFETY: Integer boxing helper; NULL is checked below.
		let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
		if boxed.is_null() {
			return Err(format!("failed to allocate {module}.{name}"));
		}
		attrs.push((intern(name), boxed));
	}
	attrs.push((intern("environ"), environ_mapping(module)?));
	if module == "posix" {
		for &(name, value) in POSIX_PRIVATE_CONSTANTS {
			let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
			if boxed.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), boxed));
		}
	}
	attrs.push((intern("error"), builtin_global("OSError")?));
	attrs.push(int_name_map_attr(module, "confstr_names", CONFSTR_NAMES)?);
	attrs.push(int_name_map_attr(module, "pathconf_names", PATHCONF_NAMES)?);
	attrs.push(int_name_map_attr(module, "sysconf_names", SYSCONF_NAMES)?);
	// SAFETY: Live builtin entry points with the runtime calling convention.
	let fspath =
		unsafe { crate::abi::pon_make_function(os_fspath as *const u8, 1, intern("fspath")) };
	if fspath.is_null() {
		return Err(format!("failed to allocate {module}.fspath"));
	}
	attrs.push((intern("fspath"), fspath));
	attrs.push(register_at_fork_attr(module)?);
	let mut stat_defaults = unsafe { [crate::abi::pon_none(), crate::abi::pon_const_bool(1)] };
	if stat_defaults.iter().any(|value| value.is_null()) {
		return Err(format!("failed to allocate {module}.stat defaults"));
	}
	attrs.push(phase_b_function_attr(
		"stat",
		os_stat,
		&["path", "dir_fd", "follow_symlinks"],
		&mut stat_defaults,
	)?);
	let mut listdir_defaults = unsafe { [pon_const_str(b".".as_ptr(), 1)] };
	if listdir_defaults.iter().any(|value| value.is_null()) {
		return Err(format!("failed to allocate {module}.listdir defaults"));
	}
	attrs.push(phase_b_function_attr("listdir", os_listdir, &["path"], &mut listdir_defaults)?);
	// `random` imports `urandom` at module top for its default seeding path.
	let urandom =
		unsafe { crate::abi::pon_make_function(os_urandom as *const u8, 1, intern("urandom")) };
	if urandom.is_null() {
		return Err(format!("failed to allocate {module}.urandom"));
	}
	attrs.push((intern("urandom"), urandom));
	// `shutil` and `pathlib._os` probe `os.stat_result` attributes at import
	// time (`hasattr(os.stat_result, 'st_file_attributes')`), so the native
	// result type is published like CPython's structseq class.
	attrs.push((intern("stat_result"), stat_result_type().cast::<PyObject>()));
	attrs.push((intern("statvfs_result"), statvfs_result_type().cast::<PyObject>()));
	attrs.push((intern("waitid_result"), waitid_result_type().cast::<PyObject>()));
	attrs.push((intern("times_result"), times_result_type().cast::<PyObject>()));
	attrs.push((intern("uname_result"), uname_result_type().cast::<PyObject>()));
	ensure_direntry_type_dict()?;
	attrs.push((intern("DirEntry"), direntry_type().cast::<PyObject>()));
	let mut scandir_defaults = unsafe { [pon_const_str(b".".as_ptr(), 1)] };
	if scandir_defaults.iter().any(|value| value.is_null()) {
		return Err(format!("failed to allocate {module}.scandir defaults"));
	}
	attrs.push(phase_b_function_attr("scandir", os_scandir, &["path"], &mut scandir_defaults)?);
	let mut chmod_defaults = unsafe { [crate::abi::pon_none(), crate::abi::pon_const_bool(1)] };
	if chmod_defaults.iter().any(|value| value.is_null()) {
		return Err(format!("failed to allocate {module}.chmod defaults"));
	}
	attrs.push(phase_b_function_attr(
		"chmod",
		os_chmod,
		&["path", "mode", "dir_fd", "follow_symlinks"],
		&mut chmod_defaults,
	)?);
	// POSIX fd/path syscall surface shared with `posix` (CPython's `os.py`
	// re-exports these names from the C `posix` module wholesale).
	for &(name, entry, arity) in SYSCALL_FUNCTIONS {
		// SAFETY: Live builtin entry points with the runtime calling convention.
		let function =
			unsafe { crate::abi::pon_make_function(entry as *const u8, arity, intern(name)) };
		if function.is_null() {
			return Err(format!("failed to allocate {module}.{name}"));
		}
		attrs.push((intern(name), function));
	}
	for &(name, entry, arity) in &[
		("WCOREDUMP", os_wcoredump as BuiltinFn, 1usize),
		("confstr", os_confstr as BuiltinFn, 1usize),
		("device_encoding", os_device_encoding as BuiltinFn, 1usize),
		("fpathconf", os_fpathconf as BuiltinFn, 2usize),
		(
			"get_terminal_size",
			os_get_terminal_size as BuiltinFn,
			crate::native::builtins_mod::VARIADIC_ARITY,
		),
		("lockf", os_lockf as BuiltinFn, 3usize),
		("login_tty", os_login_tty as BuiltinFn, 1usize),
		("mknod", os_mknod as BuiltinFn, crate::native::builtins_mod::VARIADIC_ARITY),
		("pathconf", os_pathconf as BuiltinFn, 2usize),
		("sysconf", os_sysconf as BuiltinFn, 1usize),
	] {
		let function =
			unsafe { crate::abi::pon_make_function(entry as *const u8, arity, intern(name)) };
		if function.is_null() {
			return Err(format!("failed to allocate {module}.{name}"));
		}
		attrs.push((intern(name), function));
	}
	// `terminal_size` is defined by CPython's C `posix` module, so both
	// names serve the shared class object (see the section comment for why
	// `get_terminal_size` itself stays absent).
	attrs.push((intern("terminal_size"), terminal_size_class()?));
	if module == "os" {
		// `os.py`-level surface that CPython does NOT re-export into `posix`.
		//
		// The empty capability sets are the honest non-fd contract: pon's
		// syscall wrappers implement no `dir_fd`/`fd`/`follow_symlinks`
		// variants, so membership probes (`os.stat in
		// os.supports_follow_symlinks` in tempfile, `{os.open, ...} <=
		// os.supports_dir_fd` in shutil) answer False and callers take their
		// portable fallback paths instead of the fd-relative ones.  Plain
		// mutable sets, exactly CPython's `os.py` (`supports_dir_fd = set()`
		// populated per-platform); an empty frozenset would flunk
		// `type(os.supports_dir_fd)` probes for no benefit.
		for name in
			["supports_dir_fd", "supports_effective_ids", "supports_fd", "supports_follow_symlinks"]
		{
			let mut entries: Vec<*mut PyObject> = Vec::new();
			// SAFETY: A zero-element build reads nothing through the pointer.
			let set = unsafe { crate::abi::map::pon_build_set(entries.as_mut_ptr(), 0) };
			if set.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), set));
		}
		attrs.push((intern("supports_bytes_environ"), bool_object(cfg!(unix))));
		attrs.push((intern("environb"), environb_snapshot(module)?));
		// CPython defines `_get_exports_list` in `os.py` itself (never
		// re-exported into `posix`); `socket.py` consumes it at module body:
		// `__all__.extend(os._get_exports_list(_socket))`.
		// SAFETY: Live builtin entry point with the runtime calling convention.
		let exports_list = unsafe {
			crate::abi::pon_make_function(
				os_get_exports_list as *const u8,
				1,
				intern("_get_exports_list"),
			)
		};
		if exports_list.is_null() {
			return Err(format!("failed to allocate {module}._get_exports_list"));
		}
		attrs.push((intern("_get_exports_list"), exports_list));
		// `os.py`'s fs-codec pair (`fsencode`/`fsdecode`, never re-exported
		// into `posix`); `test.support.os_helper` consumes both at module
		// body in its FS_NONASCII probe loop.
		for (name, entry) in
			[("fsencode", os_fsencode as BuiltinFn), ("fsdecode", os_fsdecode as BuiltinFn)]
		{
			// SAFETY: Live builtin entry points with the runtime calling convention.
			let function =
				unsafe { crate::abi::pon_make_function(entry as *const u8, 1, intern(name)) };
			if function.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), function));
		}
		// `os.py`'s `getenv` (never re-exported into `posix`, exactly like
		// the fs-codec pair): `environ.get(key, default)` over the LIVE
		// `os.environ` binding — see `os_getenv` for the read-through
		// contract.
		let getenv = unsafe {
			crate::abi::pon_make_function(
				os_getenv as *const u8,
				crate::native::builtins_mod::VARIADIC_ARITY,
				intern("getenv"),
			)
		};
		if getenv.is_null() {
			return Err(format!("failed to allocate {module}.getenv"));
		}
		attrs.push((intern("getenv"), getenv));
		let getenvb = unsafe {
			crate::abi::pon_make_function(
				os_getenvb as *const u8,
				crate::native::builtins_mod::VARIADIC_ARITY,
				intern("getenvb"),
			)
		};
		if getenvb.is_null() {
			return Err(format!("failed to allocate {module}.getenvb"));
		}
		attrs.push((intern("getenvb"), getenvb));
		// `os.get_exec_path(env=None)`: subprocess uses this to build the
		// `_posixsubprocess.fork_exec` executable candidate tuple when the
		// requested program name has no directory component.
		let get_exec_path = unsafe {
			crate::abi::pon_make_function(
				os_get_exec_path as *const u8,
				crate::native::builtins_mod::VARIADIC_ARITY,
				intern("get_exec_path"),
			)
		};
		if get_exec_path.is_null() {
			return Err(format!("failed to allocate {module}.get_exec_path"));
		}
		attrs.push((intern("get_exec_path"), get_exec_path));
		// `os.walk` is an os.py-level generator in CPython.  Build backends
		// prune `dirnames` in place, so the iterator below yields before
		// enqueuing top-down children and reads the same list on resume.
		let mut walk_defaults = unsafe {
			[crate::abi::pon_const_bool(1), crate::abi::pon_none(), crate::abi::pon_const_bool(0)]
		};
		if walk_defaults.iter().any(|value| value.is_null()) {
			return Err(format!("failed to allocate {module}.walk defaults"));
		}
		attrs.push(phase_b_function_attr(
			"walk",
			os_walk,
			&["top", "topdown", "onerror", "followlinks"],
			&mut walk_defaults,
		)?);
		attrs.push((intern("_walk_symlinks_as_files"), walk_symlinks_as_files()));
		let mut makedirs_defaults =
			unsafe { [crate::abi::pon_const_int(0o777), crate::abi::pon_const_bool(0)] };
		if makedirs_defaults.iter().any(|value| value.is_null()) {
			return Err(format!("failed to allocate {module}.makedirs defaults"));
		}
		attrs.push(phase_b_function_attr(
			"makedirs",
			os_makedirs,
			&["name", "mode", "exist_ok"],
			&mut makedirs_defaults,
		)?);
		// `importlib.resources._common` keeps a direct `_os_remove=os.remove`
		// reference for late finalization cleanup, so publish the CPython
		// alias alongside the underlying `unlink` syscall wrapper.
		let remove =
			unsafe { crate::abi::pon_make_function(os_unlink as *const u8, 1, intern("remove")) };
		if remove.is_null() {
			return Err(format!("failed to allocate {module}.remove"));
		}
		attrs.push((intern("remove"), remove));
		// `os.py`-level names never re-exported into `posix`: the portable
		// seek trio (see [`SEEK_MODES`]) and the null-device path (os.py
		// takes it from `posixpath.devnull`; `test.test_py_compile` probes
		// `os.path.exists(os.devnull)` at class-body time).
		for &(name, value) in SEEK_POSITIONS {
			// SAFETY: Integer boxing helper; NULL is checked below.
			let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
			if boxed.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), boxed));
		}
		for &(name, value) in OS_ONLY_CONSTANTS {
			// SAFETY: Integer boxing helper; NULL is checked below.
			let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
			if boxed.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), boxed));
		}
		attrs.push(string_attr(
			module,
			"defpath",
			if cfg!(windows) {
				".;C:\\\\bin"
			} else {
				"/bin:/usr/bin"
			},
		)?);
		attrs.push(string_attr(module, "devnull", if cfg!(windows) { "nul" } else { "/dev/null" })?);
		attrs.push(string_attr(module, "extsep", ".")?);
		for &(name, value) in &[("P_WAIT", 0i32), ("P_NOWAIT", 1i32), ("P_NOWAITO", 1i32)] {
			let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
			if boxed.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), boxed));
		}
		for &(name, entry) in &[
			("spawnv", os_spawnv as BuiltinFn),
			("spawnve", os_spawnve as BuiltinFn),
			("spawnvp", os_spawnvp as BuiltinFn),
			("spawnvpe", os_spawnvpe as BuiltinFn),
			("spawnl", os_spawnl as BuiltinFn),
			("spawnle", os_spawnle as BuiltinFn),
			("spawnlp", os_spawnlp as BuiltinFn),
			("spawnlpe", os_spawnlpe as BuiltinFn),
		] {
			let function = unsafe {
				crate::abi::pon_make_function(
					entry as *const u8,
					crate::native::builtins_mod::VARIADIC_ARITY,
					intern(name),
				)
			};
			if function.is_null() {
				return Err(format!("failed to allocate {module}.{name}"));
			}
			attrs.push((intern(name), function));
		}
		attrs.push((intern("PathLike"), pathlike_class()?));
		let process_cpu_count = unsafe {
			crate::abi::pon_make_function(os_cpu_count as *const u8, 0, intern("process_cpu_count"))
		};
		if process_cpu_count.is_null() {
			return Err(format!("failed to allocate {module}.process_cpu_count"));
		}
		attrs.push((intern("process_cpu_count"), process_cpu_count));
	}
	Ok(attrs)
}

fn phase_b_function_attr(
	name: &str,
	entry: BuiltinFn,
	names: &[&str],
	defaults: &mut [*mut PyObject],
) -> Result<(u32, *mut PyObject), String> {
	let interned_names: Vec<u32> = names.iter().map(|name| intern(name)).collect();
	let params = ParamSpec {
		names:                 if interned_names.is_empty() {
			std::ptr::null()
		} else {
			interned_names.as_ptr()
		},
		total_param_count:     interned_names.len() as u32,
		positional_only_count: 0,
		positional_count:      interned_names.len() as u32,
		keyword_only_count:    0,
		varargs_name:          0,
		varkw_name:            0,
	};
	let code = CodeInfo {
		entry:         entry as *const u8,
		params:        &params,
		name_interned: intern(name),
		n_locals:      0,
		n_feedback:    0,
		flags:         0,
	};
	let function = unsafe {
		crate::abi::call::pon_make_function_full(
			&code,
			if defaults.is_empty() {
				std::ptr::null_mut()
			} else {
				defaults.as_mut_ptr()
			},
			defaults.len(),
			std::ptr::null(),
			std::ptr::null_mut(),
			0,
			std::ptr::null(),
			std::ptr::null_mut(),
			0,
		)
	};
	if function.is_null() {
		return Err(format!("failed to allocate os.{name}"));
	}
	crate::types::function::mark_native_function(function);
	Ok((intern(name), function))
}

fn register_at_fork_attr(module: &str) -> Result<(u32, *mut PyObject), String> {
	let names = [intern("before"), intern("after_in_child"), intern("after_in_parent")];
	let params = ParamSpec {
		names:                 names.as_ptr(),
		total_param_count:     names.len() as u32,
		positional_only_count: 0,
		positional_count:      0,
		keyword_only_count:    names.len() as u32,
		varargs_name:          0,
		varkw_name:            0,
	};
	let code = CodeInfo {
		entry:         os_register_at_fork as *const u8,
		params:        &params,
		name_interned: intern("register_at_fork"),
		n_locals:      0,
		n_feedback:    0,
		flags:         0,
	};
	let mut kwdefaults =
		unsafe { [crate::abi::pon_none(), crate::abi::pon_none(), crate::abi::pon_none()] };
	if kwdefaults.iter().any(|value| value.is_null()) {
		return Err(format!("failed to allocate {module}.register_at_fork defaults"));
	}
	let function = unsafe {
		crate::abi::call::pon_make_function_full(
			&code,
			std::ptr::null_mut(),
			0,
			names.as_ptr(),
			kwdefaults.as_mut_ptr(),
			kwdefaults.len(),
			std::ptr::null(),
			std::ptr::null_mut(),
			0,
		)
	};
	if function.is_null() {
		return Err(format!("failed to allocate {module}.register_at_fork"));
	}
	crate::types::function::mark_native_function(function);
	Ok((intern("register_at_fork"), function))
}

/// `os._get_exports_list(module)`: CPython os.py's own helper, served
/// natively because pon's `os` is a curated seed rather than the source
/// module.  `list(module.__all__)` when the module defines `__all__`, else
/// the sorted non-underscore namespace names — exactly os.py's
/// `[n for n in dir(module) if n[0] != '_']`.
unsafe extern "C" fn os_get_exports_list(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::return_null_with_error("os._get_exports_list expected one argument");
	}
	// SAFETY: One live argument slot per the check above.
	let module = crate::tag::untag_arg(unsafe { *argv });
	// `__all__` arm: any iterable, materialized as a fresh list (CPython's
	// `list(module.__all__)`).
	if let Some(all) = unsafe { super::builtins_batch::try_get_attr(module, "__all__") } {
		return match super::builtins_batch::collect_iterable(all) {
			// SAFETY: List builder reads exactly `len` live slots.
			Ok(mut values) => unsafe {
				crate::abi::seq::pon_build_list(values.as_mut_ptr(), values.len())
			},
			// SAFETY: Typed raise helper.
			Err(message) => unsafe {
				crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len())
			},
		};
	}
	// `dir()` fallback arm: modules enumerate their registered namespace
	// dict (the `builtin_dir` module arm); anything else walks the MRO.
	let names = match crate::import::module_namespace_for_object(module) {
		Some(Ok(namespace)) => {
			match unsafe { super::builtins_batch::names_from_mapping(namespace) } {
				Ok(names) => names,
				Err(message) => return crate::abi::return_null_with_error(message),
			}
		},
		Some(Err(message)) => return crate::abi::return_null_with_error(message),
		None => super::builtins_batch::names_for_object(module),
	};
	let mut names: Vec<String> = names
		.into_iter()
		.filter(|name| !name.starts_with('_'))
		.collect();
	names.sort();
	names.dedup();
	super::builtins_batch::build_str_list(names)
}

/// True when `raw` carries a `str` or `bytes` payload, subclass instances
/// included (CPython `PyUnicode_Check`/`PyBytes_Check`).
pub(super) fn type_derives_str_or_bytes(raw: *mut PyObject) -> bool {
	// SAFETY: Callers proved `raw` is a live heap pointer.
	if unsafe { crate::types::type_::unicode_text(raw) }.is_some() {
		return true;
	}
	if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
		return true;
	}
	// bytes subclasses wrap their payload in a subclass carrier.
	unsafe { crate::types::type_::payload_subclass_value(raw) }.is_some_and(|payload| {
		payload != raw
			&& !payload.is_null()
			&& crate::types::bytes_::is_bytes_type(unsafe { (*payload).ob_type })
	})
}

/// `os.fspath(path)`: str/bytes pass through unchanged; other objects defer
/// to their type's `__fspath__`; everything else raises CPython's TypeError.
unsafe extern "C" fn os_fspath(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::return_null_with_error("os.fspath expected one argument");
	}
	// SAFETY: One live argument slot per the check above.
	let path = unsafe { *argv };
	let raw = crate::tag::untag_arg(path);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		// str/bytes pass through unchanged, SUBCLASS instances included
		// (CPython `PyUnicode_Check`; meson's OptionString is a str
		// subclass fed straight into path joins).
		if type_derives_str_or_bytes(raw) {
			return path;
		}
		// SAFETY: Live header per the checks above.
		let ty = unsafe { (*raw).ob_type.cast_mut() };
		let hook = unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) };
		if !hook.is_null() {
			let bound = unsafe { crate::descr::descriptor_get(hook, raw, ty) };
			if bound.is_null() {
				return std::ptr::null_mut();
			}
			// SAFETY: Call helper follows the NULL-sentinel error contract.
			return unsafe { crate::abi::pon_call(bound, std::ptr::null_mut(), 0) };
		}
	}
	let display = if raw.is_null() {
		"NoneType"
	} else if crate::tag::is_small_int(raw) {
		"int"
	} else {
		// SAFETY: Heap pointer with a live header after the tag checks.
		unsafe { crate::types::dict::type_name(raw) }.unwrap_or("object")
	};
	let message = format!("expected str, bytes or os.PathLike object, not {display}");
	// SAFETY: Typed raise helper.
	unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

/// `os.fsencode(filename)`: `os.py`'s fs-codec pair, served natively.
/// fspath coercion first (str/bytes pass, `__fspath__` defers), then str
/// encodes with the filesystem encoding.  Divergence: pon's filesystem
/// encoding is strict UTF-8 with no `surrogateescape` — pon str never
/// carries lone surrogates, so the encode step itself is total.
unsafe extern "C" fn os_fsencode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Delegated coercion under the caller's own live argv contract.
	let coerced = unsafe { os_fspath(argv, argc) };
	if coerced.is_null() {
		return std::ptr::null_mut();
	}
	let raw = crate::tag::untag_arg(coerced);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		// SAFETY: Heap pointer with a live header after the tag checks.
		if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
			// SAFETY: Bytes allocation helper follows the NULL-sentinel contract.
			return unsafe { crate::abi::str_::pon_const_bytes(text.as_ptr(), text.len()) };
		}
		// SAFETY: Live header per the checks above.
		if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
			return coerced;
		}
	}
	fs_codec_hook_type_error("fsencode", raw)
}

/// `os.fsdecode(filename)`: bytes decode with the filesystem encoding
/// (strict UTF-8 — see [`os_fsencode`] for the surrogateescape divergence),
/// str passes through.
unsafe extern "C" fn os_fsdecode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Delegated coercion under the caller's own live argv contract.
	let coerced = unsafe { os_fspath(argv, argc) };
	if coerced.is_null() {
		return std::ptr::null_mut();
	}
	let raw = crate::tag::untag_arg(coerced);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		// SAFETY: Heap pointer with a live header after the tag checks.
		if unsafe { crate::types::type_::unicode_text(raw) }.is_some() {
			return coerced;
		}
		// SAFETY: Live header per the checks above.
		if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
			// SAFETY: The type check proved PyBytes layout.
			let payload = unsafe { (*raw.cast::<crate::types::bytes_::PyBytes>()).as_slice() };
			return match super::codecs::utf8_decode_core(payload, "strict", true) {
				Ok((text, _)) => {
					// SAFETY: String allocation helper follows the NULL-sentinel contract.
					unsafe { pon_const_str(text.as_ptr(), text.len()) }
				},
				Err(error) => error.raise(),
			};
		}
	}
	fs_codec_hook_type_error("fsdecode", raw)
}

/// TypeError for a `__fspath__` hook that returned a non-str/bytes object.
/// Direct non-path arguments already raised inside the fspath coercion;
/// CPython raises this shape check inside `fspath` itself (`expected
/// X.__fspath__() to return str or bytes, not Y`), pon's message names the
/// consuming codec instead because the coercion returns hook results
/// unvalidated.
fn fs_codec_hook_type_error(what: &str, raw: *mut PyObject) -> *mut PyObject {
	let display = if raw.is_null() {
		"NoneType"
	} else if crate::tag::is_small_int(raw) {
		"int"
	} else {
		// SAFETY: Heap pointer with a live header per the caller's checks.
		unsafe { crate::types::dict::type_name(raw) }.unwrap_or("object")
	};
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		&format!("os.{what}: __fspath__() must return str or bytes, not {display}"),
	)
}

/// `os.urandom(size)`: `size` cryptographically random bytes from the OS
/// entropy source (`getentropy(2)`, chunked at its 256-byte call limit).
unsafe extern "C" fn os_urandom(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::return_null_with_error("os.urandom expected one argument");
	}
	// SAFETY: One live argument slot per the check above.
	let size = crate::tag::untag_arg(unsafe { *argv });
	// SAFETY: Type probe tolerates any live object.
	let Some(size) = (unsafe { crate::types::int::to_bigint_including_bool(size) }) else {
		let message = "os.urandom expected an int argument";
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	};
	use num_traits::{Signed, ToPrimitive};
	if size.is_negative() {
		let message = "negative argument not allowed";
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
	}
	let Some(size) = size.to_usize() else {
		let message = "os.urandom size out of range";
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) };
	};
	let mut bytes = vec![0u8; size];
	for chunk in bytes.chunks_mut(256) {
		// SAFETY: `chunk` is a live writable buffer of the passed length.
		if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
			let message = "getentropy failed";
			// SAFETY: Typed raise helper.
			return unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) };
		}
	}
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { crate::abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

/// `os.stat_result` shape: serve the POSIX fields consumed by the vendored
/// stdlib and Meson (`st_size`/`st_mtime`, permission/owner bits, and stable
/// file identity). Unknown attributes still raise AttributeError so the next
/// frontier is loud, not silently wrong (`_pyio` reads `st_blksize` through
/// `getattr(..., 0)`, which that AttributeError serves correctly).
#[repr(C)]
struct PyStatResult {
	ob_base:     crate::object::PyObjectHeader,
	st_size:     i64,
	st_atime:    f64,
	st_mtime:    f64,
	st_ctime:    f64,
	st_atime_ns: i64,
	st_mtime_ns: i64,
	st_ctime_ns: i64,
	st_mode:     i64,
	st_ino:      i64,
	st_dev:      i64,
	st_nlink:    i64,
	st_uid:      i64,
	st_gid:      i64,
}

fn stat_result_type() -> *mut crate::object::PyType {
	static STAT_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"os.stat_result",
			std::mem::size_of::<PyStatResult>(),
		);
		ty.tp_getattro = Some(stat_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*STAT_TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn stat_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		crate::thread_state::pon_err_set("attribute name must be str");
		return std::ptr::null_mut();
	};
	let stat = object.cast::<PyStatResult>();
	match name_text {
		// SAFETY: Receivers of this getattro are PyStatResult allocations.
		"st_size" => unsafe { crate::abi::pon_const_int((*stat).st_size) },
		"st_atime" => unsafe { crate::abi::number::pon_const_float((*stat).st_atime) },
		"st_mtime" => unsafe { crate::abi::number::pon_const_float((*stat).st_mtime) },
		"st_ctime" => unsafe { crate::abi::number::pon_const_float((*stat).st_ctime) },
		"st_atime_ns" => unsafe { crate::abi::pon_const_int((*stat).st_atime_ns) },
		"st_mtime_ns" => unsafe { crate::abi::pon_const_int((*stat).st_mtime_ns) },
		"st_ctime_ns" => unsafe { crate::abi::pon_const_int((*stat).st_ctime_ns) },
		"st_mode" => unsafe { crate::abi::pon_const_int((*stat).st_mode) },
		"st_ino" => unsafe { crate::abi::pon_const_int((*stat).st_ino) },
		"st_dev" => unsafe { crate::abi::pon_const_int((*stat).st_dev) },
		"st_nlink" => unsafe { crate::abi::pon_const_int((*stat).st_nlink) },
		"st_uid" => unsafe { crate::abi::pon_const_int((*stat).st_uid) },
		"st_gid" => unsafe { crate::abi::pon_const_int((*stat).st_gid) },
		_ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

/// `os.stat(path, *, dir_fd=None, follow_symlinks=True)`: follows symlinks by
/// default and honors the portable no-follow spelling through `lstat` metadata.
/// fd-relative lookup is not in pon's advertised capability set.
unsafe extern "C" fn os_stat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if !(1..=3).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.stat expected one argument");
	}
	let path_text = match path_arg(args[0], "stat") {
		Ok(path) => path,
		Err(error) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 1, "stat") {
		return error;
	}
	let follow_symlinks = match optional_arg(args, 2) {
		Some(value) => match truth_arg(value) {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => true,
	};
	let result = if follow_symlinks {
		std::fs::metadata(&path_text)
	} else {
		std::fs::symlink_metadata(&path_text)
	};
	match result {
		Ok(metadata) => stat_result_object(&metadata),
		Err(error) => {
			raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(path_text.as_str()))
		},
	}
}

/// Boxes host metadata as the `os.stat_result` shape shared by `os.stat`
/// and `os.lstat`.
fn stat_result_object(metadata: &std::fs::Metadata) -> *mut PyObject {
	stat_result_from_fields(stat_fields_from_metadata(metadata))
}

fn stat_fields_from_metadata(metadata: &std::fs::Metadata) -> StatFields {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		StatFields {
			st_size:     stat_i64(metadata.len()),
			st_atime:    stat_timestamp(metadata.atime(), metadata.atime_nsec()),
			st_mtime:    stat_timestamp(metadata.mtime(), metadata.mtime_nsec()),
			st_ctime:    stat_timestamp(metadata.ctime(), metadata.ctime_nsec()),
			st_atime_ns: stat_timestamp_ns(metadata.atime(), metadata.atime_nsec()),
			st_mtime_ns: stat_timestamp_ns(metadata.mtime(), metadata.mtime_nsec()),
			st_ctime_ns: stat_timestamp_ns(metadata.ctime(), metadata.ctime_nsec()),
			st_mode:     stat_i64(metadata.mode()),
			st_ino:      stat_i64(metadata.ino()),
			st_dev:      stat_i64(metadata.dev()),
			st_nlink:    stat_i64(metadata.nlink()),
			st_uid:      stat_i64(metadata.uid()),
			st_gid:      stat_i64(metadata.gid()),
		}
	}
	#[cfg(not(unix))]
	{
		StatFields {
			st_size:     stat_i64(metadata.len()),
			st_atime:    metadata
				.accessed()
				.ok()
				.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
				.map_or(0.0, |duration| duration.as_secs_f64()),
			st_mtime:    metadata
				.modified()
				.ok()
				.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
				.map_or(0.0, |duration| duration.as_secs_f64()),
			st_ctime:    metadata
				.created()
				.ok()
				.and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
				.map_or(0.0, |duration| duration.as_secs_f64()),
			st_atime_ns: metadata.accessed().ok().map_or(0, system_time_ns),
			st_mtime_ns: metadata.modified().ok().map_or(0, system_time_ns),
			st_ctime_ns: metadata.created().ok().map_or(0, system_time_ns),
			st_mode:     0,
			st_ino:      0,
			st_dev:      0,
			st_nlink:    0,
			st_uid:      0,
			st_gid:      0,
		}
	}
}

#[derive(Clone, Copy)]
struct StatFields {
	st_size:     i64,
	st_atime:    f64,
	st_mtime:    f64,
	st_ctime:    f64,
	st_atime_ns: i64,
	st_mtime_ns: i64,
	st_ctime_ns: i64,
	st_mode:     i64,
	st_ino:      i64,
	st_dev:      i64,
	st_nlink:    i64,
	st_uid:      i64,
	st_gid:      i64,
}

fn stat_i64<T>(value: T) -> i64
where
	i64: TryFrom<T>,
{
	i64::try_from(value).unwrap_or(i64::MAX)
}

#[allow(clippy::cast_precision_loss)]
fn stat_timestamp(seconds: i64, nanoseconds: i64) -> f64 {
	seconds as f64 + nanoseconds as f64 * 1e-9
}

fn stat_timestamp_ns(seconds: i64, nanoseconds: i64) -> i64 {
	seconds
		.saturating_mul(1_000_000_000)
		.saturating_add(nanoseconds)
}

#[cfg(not(unix))]
fn system_time_ns(time: std::time::SystemTime) -> i64 {
	time
		.duration_since(std::time::UNIX_EPOCH)
		.ok()
		.and_then(|duration| i64::try_from(duration.as_nanos()).ok())
		.unwrap_or(0)
}

/// Boxes explicit field values as an `os.stat_result`; shared by the
/// metadata path above and the raw `fstat(2)` path below.
fn stat_result_from_fields(fields: StatFields) -> *mut PyObject {
	Box::into_raw(Box::new(PyStatResult {
		ob_base:     crate::object::PyObjectHeader::new(stat_result_type()),
		st_size:     fields.st_size,
		st_atime:    fields.st_atime,
		st_mtime:    fields.st_mtime,
		st_ctime:    fields.st_ctime,
		st_atime_ns: fields.st_atime_ns,
		st_mtime_ns: fields.st_mtime_ns,
		st_ctime_ns: fields.st_ctime_ns,
		st_mode:     fields.st_mode,
		st_ino:      fields.st_ino,
		st_dev:      fields.st_dev,
		st_nlink:    fields.st_nlink,
		st_uid:      fields.st_uid,
		st_gid:      fields.st_gid,
	}))
	.cast::<PyObject>()
}

/// `os.fstat(fd)` over `fstat(2)`: the stat_result for an open descriptor
/// (`_pyio.FileIO.__init__` probes `S_ISDIR(st_mode)`; `netrc`'s security
/// check compares `st_uid` against `os.getuid()`).
unsafe extern "C" fn os_fstat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.fstat expected one argument");
	}
	let fd = match int_arg(args[0], "fstat fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
	// SAFETY: `raw` is a live out-buffer; failure reports through errno below.
	if unsafe { libc::fstat(fd as libc::c_int, raw.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: fstat(2) success fills the whole struct.
	let raw = unsafe { raw.assume_init() };
	let fields = StatFields {
		st_size:     stat_i64(raw.st_size),
		st_atime:    stat_timestamp(raw.st_atime, raw.st_atime_nsec),
		st_mtime:    stat_timestamp(raw.st_mtime, raw.st_mtime_nsec),
		st_ctime:    stat_timestamp(raw.st_ctime, raw.st_ctime_nsec),
		st_atime_ns: stat_timestamp_ns(raw.st_atime, raw.st_atime_nsec),
		st_mtime_ns: stat_timestamp_ns(raw.st_mtime, raw.st_mtime_nsec),
		st_ctime_ns: stat_timestamp_ns(raw.st_ctime, raw.st_ctime_nsec),
		st_mode:     stat_i64(raw.st_mode),
		st_ino:      stat_i64(raw.st_ino),
		st_dev:      stat_i64(raw.st_dev),
		st_nlink:    stat_i64(raw.st_nlink),
		st_uid:      stat_i64(raw.st_uid),
		st_gid:      stat_i64(raw.st_gid),
	};
	stat_result_from_fields(fields)
}

/// `os.chmod(path, mode, follow_symlinks=True)` over `chmod(2)`
/// (`test.support.os_helper.can_chmod` round-trips it against
/// `os.stat().st_mode`).  The no-follow variant is not in pon's advertised
/// capability set, so direct false requests fail loudly.
unsafe extern "C" fn os_chmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(2..=4).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.chmod expected two arguments (path, mode)");
	}
	let path = match path_arg(args[0], "chmod") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match int_arg(args[1], "chmod mode") {
		Ok(mode) => mode,
		Err(error) => return error,
	};
	// CPython: `chmod(path, mode, *, dir_fd=None, follow_symlinks=True)` —
	// a real dir_fd routes through fchmodat(2).
	let mut dir_fd: Option<i64> = None;
	if let Some(dir_fd_arg) = optional_arg(args, 2) {
		dir_fd = match int_arg(dir_fd_arg, "chmod dir_fd") {
			Ok(value) => Some(value),
			Err(error) => return error,
		};
	}
	if let Some(follow_arg) = optional_arg(args, 3) {
		let follow = match truth_arg(follow_arg) {
			Ok(value) => value,
			Err(error) => return error,
		};
		if !follow {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::NotImplementedError,
				"chmod: follow_symlinks unavailable on this platform",
			);
		}
	}
	if let Some(fd) = dir_fd {
		let c_path = match c_path(&path) {
			Ok(c_path) => c_path,
			Err(error) => return error,
		};
		// SAFETY: `c_path` is NUL-terminated; fd validity is the caller's contract.
		if unsafe { libc::fchmodat(fd as libc::c_int, c_path.as_ptr(), mode as libc::mode_t, 0) } < 0
		{
			return raise_errno(last_errno(), Some(&path));
		}
		// SAFETY: Singleton accessor.
		return unsafe { crate::abi::pon_none() };
	}
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::chmod(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.umask(mask)` sets the process umask and returns the previous mask.
unsafe extern "C" fn os_umask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.umask expected one argument");
	}
	let mask = match int_arg(args[0], "umask mask") {
		Ok(mask) => mask,
		Err(error) => return error,
	};
	// SAFETY: umask(2) cannot fail; it returns the previous process mask.
	let previous = unsafe { libc::umask(mask as libc::mode_t) };
	unsafe { crate::abi::pon_const_int(i64::from(previous)) }
}

/// `os.access(path, mode)` over `access(2)`: reports whether the process can
/// access `path` under `mode` (an `F_OK`/`R_OK`/`W_OK`/`X_OK` combination).
/// Never raises for an inaccessible path — a failing check returns `False`,
/// matching CPython.
unsafe extern "C" fn os_access(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.access expected two arguments (path, mode)");
	}
	let path = match path_arg(args[0], "access") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match int_arg(args[1], "access mode") {
		Ok(mode) => mode,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated; `access(2)` returning nonzero (with
	// errno set) means "not accessible", which CPython folds into False.
	let ok = unsafe { libc::access(c_path.as_ptr(), mode as libc::c_int) } == 0;
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::number::pon_const_bool(i32::from(ok)) }
}

/// `os.getuid()` over `getuid(2)` (`netrc._can_security_check` gates on its
/// presence; the check itself compares it to the file owner).
unsafe extern "C" fn os_getuid(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	// SAFETY: getuid(2) cannot fail; integer boxing follows the NULL-sentinel
	// contract.
	unsafe { crate::abi::pon_const_int(i64::from(libc::getuid())) }
}

/// `os.isatty(fd)` over `isatty(3)` (`_pyio.open`'s default-buffering path
/// probes `raw.isatty()` to pick line buffering).
unsafe extern "C" fn os_isatty(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.isatty expected one argument");
	}
	let fd = match int_arg(args[0], "isatty fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	// SAFETY: Plain fd probe; a non-tty (or bad fd) answers 0 with errno,
	// which CPython folds into False rather than raising.
	let is_tty = unsafe { libc::isatty(fd as libc::c_int) } != 0;
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::number::pon_const_bool(i32::from(is_tty)) }
}

/// `os.path`: CPython's `os.py` publishes `sys.modules['os.path'] =
/// posixpath`; the native seed mirrors that aliasing lazily by resolving the
/// vendored `posixpath` source module on first import.  The importer then
/// binds it under both names and as the parent's `path` attribute.
pub(super) fn make_path_module() -> Result<*mut PyObject, String> {
	// SAFETY: Import entry point follows the NULL-sentinel error contract.
	let module = unsafe {
		crate::import::pon_import_name(
			intern(if cfg!(windows) { "ntpath" } else { "posixpath" }),
			std::ptr::null(),
			0,
			0,
		)
	};
	if module.is_null() {
		return Err("failed to import posixpath for os.path".to_owned());
	}
	Ok(module)
}

fn os_name() -> &'static str {
	if cfg!(windows) { "nt" } else { "posix" }
}

fn builtin_global(name: &str) -> Result<*mut PyObject, String> {
	crate::abi::runtime_global(intern(name)).ok_or_else(|| format!("missing builtin {name}"))
}

fn string_attr(module: &str, name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate {module}.{name}"))
}
fn int_name_map_attr(
	module: &str,
	name: &str,
	pairs: &[(&str, i32)],
) -> Result<(u32, *mut PyObject), String> {
	let mut objects = Vec::with_capacity(pairs.len() * 2);
	for &(key, value) in pairs {
		let key_obj = unsafe { pon_const_str(key.as_ptr(), key.len()) };
		let value_obj = unsafe { crate::abi::pon_const_int(i64::from(value)) };
		if key_obj.is_null() || value_obj.is_null() {
			return Err(format!("failed to allocate {module}.{name} entry"));
		}
		objects.push(key_obj);
		objects.push(value_obj);
	}
	let pair_count = objects.len() / 2;
	let map = unsafe { crate::abi::map::pon_build_map(objects.as_mut_ptr(), pair_count) };
	if map.is_null() {
		return Err(format!("failed to allocate {module}.{name}"));
	}
	Ok((intern(name), map))
}

/// `open(2)` flag constants: the portable-POSIX set plus cfg-gated host
/// extras, sorted by name. Values come from libc, so they always match the
/// host CPython's.
const OPEN_FLAGS: &[(&str, i32)] = &[
	("O_ACCMODE", libc::O_ACCMODE),
	("O_APPEND", libc::O_APPEND),
	("O_ASYNC", libc::O_ASYNC),
	("O_CLOEXEC", libc::O_CLOEXEC),
	("O_CREAT", libc::O_CREAT),
	("O_DIRECTORY", libc::O_DIRECTORY),
	("O_DSYNC", libc::O_DSYNC),
	#[cfg(target_os = "macos")]
	("O_EVTONLY", libc::O_EVTONLY),
	("O_EXCL", libc::O_EXCL),
	#[cfg(target_os = "macos")]
	("O_EXEC", libc::O_EXEC),
	#[cfg(target_os = "macos")]
	("O_EXLOCK", libc::O_EXLOCK),
	("O_FSYNC", libc::O_FSYNC),
	("O_NDELAY", libc::O_NDELAY),
	("O_NOCTTY", libc::O_NOCTTY),
	("O_NOFOLLOW", libc::O_NOFOLLOW),
	#[cfg(target_os = "macos")]
	("O_NOFOLLOW_ANY", libc::O_NOFOLLOW_ANY),
	("O_NONBLOCK", libc::O_NONBLOCK),
	("O_RDONLY", libc::O_RDONLY),
	("O_RDWR", libc::O_RDWR),
	#[cfg(target_os = "macos")]
	("O_SEARCH", libc::O_SEARCH),
	#[cfg(target_os = "macos")]
	("O_SHLOCK", libc::O_SHLOCK),
	("O_SYNC", libc::O_SYNC),
	#[cfg(target_os = "macos")]
	("O_SYMLINK", libc::O_SYMLINK),
	("O_TRUNC", libc::O_TRUNC),
	("O_WRONLY", libc::O_WRONLY),
];

/// `os.access(2)` mode constants (`shutil.which`'s default `mode` argument
/// is evaluated at module body: `mode=os.F_OK | os.X_OK`).
const ACCESS_FLAGS: &[(&str, i32)] =
	&[("F_OK", libc::F_OK), ("R_OK", libc::R_OK), ("W_OK", libc::W_OK), ("X_OK", libc::X_OK)];

/// `waitpid(2)` option constants.
const WAIT_OPTIONS: &[(&str, i32)] =
	&[("WCONTINUED", libc::WCONTINUED), ("WNOHANG", libc::WNOHANG), ("WUNTRACED", libc::WUNTRACED)];

/// POSIX/Darwin constants exported by CPython's C `posix` module and then
/// re-exported by `os.py` when the name is public.
const POSIX_CONSTANTS: &[(&str, i32)] = &[
	("CLD_CONTINUED", libc::CLD_CONTINUED),
	("CLD_DUMPED", libc::CLD_DUMPED),
	("CLD_EXITED", libc::CLD_EXITED),
	("CLD_KILLED", libc::CLD_KILLED),
	("CLD_STOPPED", libc::CLD_STOPPED),
	("CLD_TRAPPED", libc::CLD_TRAPPED),
	// sysexits.h values surfaced by CPython on Darwin.
	("EX_CANTCREAT", 73),
	("EX_CONFIG", 78),
	("EX_DATAERR", 65),
	("EX_IOERR", 74),
	("EX_NOHOST", 68),
	("EX_NOINPUT", 66),
	("EX_NOPERM", 77),
	("EX_NOUSER", 67),
	("EX_OK", 0),
	("EX_OSERR", 71),
	("EX_OSFILE", 72),
	("EX_PROTOCOL", 76),
	("EX_SOFTWARE", 70),
	("EX_TEMPFAIL", 75),
	("EX_UNAVAILABLE", 69),
	("EX_USAGE", 64),
	("F_LOCK", libc::F_LOCK),
	("F_TEST", libc::F_TEST),
	("F_TLOCK", libc::F_TLOCK),
	("F_ULOCK", libc::F_ULOCK),
	// Compile-time NGROUPS_MAX (Darwin 16, Linux 65536) is not exposed by libc.
	#[cfg(target_os = "macos")]
	("NGROUPS_MAX", 16),
	#[cfg(not(target_os = "macos"))]
	("NGROUPS_MAX", 65536),
	// posix_spawn file-action tags from Darwin spawn.h.
	("POSIX_SPAWN_CLOSE", 1),
	("POSIX_SPAWN_DUP2", 2),
	("POSIX_SPAWN_OPEN", 0),
	#[cfg(target_os = "macos")]
	("PRIO_DARWIN_BG", libc::PRIO_DARWIN_BG),
	#[cfg(target_os = "macos")]
	("PRIO_DARWIN_NONUI", libc::PRIO_DARWIN_NONUI),
	#[cfg(target_os = "macos")]
	("PRIO_DARWIN_PROCESS", libc::PRIO_DARWIN_PROCESS),
	#[cfg(target_os = "macos")]
	("PRIO_DARWIN_THREAD", libc::PRIO_DARWIN_THREAD),
	("PRIO_PGRP", libc::PRIO_PGRP as i32),
	("PRIO_PROCESS", libc::PRIO_PROCESS as i32),
	("PRIO_USER", libc::PRIO_USER as i32),
	("P_ALL", libc::P_ALL as i32),
	("P_PGID", libc::P_PGID as i32),
	("P_PID", libc::P_PID as i32),
	("RTLD_GLOBAL", libc::RTLD_GLOBAL),
	("RTLD_LAZY", libc::RTLD_LAZY),
	("RTLD_LOCAL", libc::RTLD_LOCAL),
	("RTLD_NODELETE", libc::RTLD_NODELETE),
	("RTLD_NOLOAD", libc::RTLD_NOLOAD),
	("RTLD_NOW", libc::RTLD_NOW),
	// sched.h policy constants; the Darwin values are missing from libc.
	#[cfg(target_os = "macos")]
	("SCHED_FIFO", 4),
	#[cfg(target_os = "macos")]
	("SCHED_OTHER", 1),
	#[cfg(target_os = "macos")]
	("SCHED_RR", 2),
	#[cfg(not(target_os = "macos"))]
	("SCHED_FIFO", libc::SCHED_FIFO),
	#[cfg(not(target_os = "macos"))]
	("SCHED_OTHER", libc::SCHED_OTHER),
	#[cfg(not(target_os = "macos"))]
	("SCHED_RR", libc::SCHED_RR),
	("ST_NOSUID", libc::ST_NOSUID as i32),
	("ST_RDONLY", libc::ST_RDONLY as i32),
	("TMP_MAX", libc::TMP_MAX as i32),
	("WEXITED", libc::WEXITED),
	("WNOWAIT", libc::WNOWAIT),
	("WSTOPPED", libc::WSTOPPED),
];

/// Private Darwin constants present on `posix` but not re-exported by
/// `os.py`; empty elsewhere.
const POSIX_PRIVATE_CONSTANTS: &[(&str, i32)] = &[
	#[cfg(target_os = "macos")]
	("_COPYFILE_ACL", libc::COPYFILE_ACL as i32),
	#[cfg(target_os = "macos")]
	("_COPYFILE_DATA", libc::COPYFILE_DATA as i32),
	#[cfg(target_os = "macos")]
	("_COPYFILE_STAT", libc::COPYFILE_STAT as i32),
	#[cfg(target_os = "macos")]
	("_COPYFILE_XATTR", libc::COPYFILE_XATTR as i32),
];

/// `os.py`-only constants.
const OS_ONLY_CONSTANTS: &[(&str, i32)] = &[("P_NOWAIT", 1), ("P_NOWAITO", 1), ("P_WAIT", 0)];
#[cfg(target_os = "macos")]
const CONFSTR_NAMES: &[(&str, i32)] = &[
	("CS_PATH", 1),
	("CS_XBS5_ILP32_OFF32_CFLAGS", 20),
	("CS_XBS5_ILP32_OFF32_LDFLAGS", 21),
	("CS_XBS5_ILP32_OFF32_LIBS", 22),
	("CS_XBS5_ILP32_OFF32_LINTFLAGS", 23),
	("CS_XBS5_ILP32_OFFBIG_CFLAGS", 24),
	("CS_XBS5_ILP32_OFFBIG_LDFLAGS", 25),
	("CS_XBS5_ILP32_OFFBIG_LIBS", 26),
	("CS_XBS5_ILP32_OFFBIG_LINTFLAGS", 27),
	("CS_XBS5_LP64_OFF64_CFLAGS", 28),
	("CS_XBS5_LP64_OFF64_LDFLAGS", 29),
	("CS_XBS5_LP64_OFF64_LIBS", 30),
	("CS_XBS5_LP64_OFF64_LINTFLAGS", 31),
	("CS_XBS5_LPBIG_OFFBIG_CFLAGS", 32),
	("CS_XBS5_LPBIG_OFFBIG_LDFLAGS", 33),
	("CS_XBS5_LPBIG_OFFBIG_LIBS", 34),
	("CS_XBS5_LPBIG_OFFBIG_LINTFLAGS", 35),
];

#[cfg(not(target_os = "macos"))]
const CONFSTR_NAMES: &[(&str, i32)] = &[
	("CS_GNU_LIBC_VERSION", 2),
	("CS_GNU_LIBPTHREAD_VERSION", 3),
	("CS_LFS64_CFLAGS", 1004),
	("CS_LFS64_LDFLAGS", 1005),
	("CS_LFS64_LIBS", 1006),
	("CS_LFS64_LINTFLAGS", 1007),
	("CS_LFS_CFLAGS", 1000),
	("CS_LFS_LDFLAGS", 1001),
	("CS_LFS_LIBS", 1002),
	("CS_LFS_LINTFLAGS", 1003),
	("CS_PATH", 0),
	("CS_V6_WIDTH_RESTRICTED_ENVS", 1),
	("CS_XBS5_ILP32_OFF32_CFLAGS", 1100),
	("CS_XBS5_ILP32_OFF32_LDFLAGS", 1101),
	("CS_XBS5_ILP32_OFF32_LIBS", 1102),
	("CS_XBS5_ILP32_OFF32_LINTFLAGS", 1103),
	("CS_XBS5_ILP32_OFFBIG_CFLAGS", 1104),
	("CS_XBS5_ILP32_OFFBIG_LDFLAGS", 1105),
	("CS_XBS5_ILP32_OFFBIG_LIBS", 1106),
	("CS_XBS5_ILP32_OFFBIG_LINTFLAGS", 1107),
	("CS_XBS5_LP64_OFF64_CFLAGS", 1108),
	("CS_XBS5_LP64_OFF64_LDFLAGS", 1109),
	("CS_XBS5_LP64_OFF64_LIBS", 1110),
	("CS_XBS5_LP64_OFF64_LINTFLAGS", 1111),
	("CS_XBS5_LPBIG_OFFBIG_CFLAGS", 1112),
	("CS_XBS5_LPBIG_OFFBIG_LDFLAGS", 1113),
	("CS_XBS5_LPBIG_OFFBIG_LIBS", 1114),
	("CS_XBS5_LPBIG_OFFBIG_LINTFLAGS", 1115),
];

const PATHCONF_NAMES: &[(&str, i32)] = &[
	("PC_ALLOC_SIZE_MIN", 16),
	("PC_ASYNC_IO", 17),
	("PC_CHOWN_RESTRICTED", 7),
	("PC_FILESIZEBITS", 18),
	("PC_LINK_MAX", 1),
	("PC_MAX_CANON", 2),
	("PC_MAX_INPUT", 3),
	("PC_MIN_HOLE_SIZE", 27),
	("PC_NAME_MAX", 4),
	("PC_NO_TRUNC", 8),
	("PC_PATH_MAX", 5),
	("PC_PIPE_BUF", 6),
	("PC_PRIO_IO", 19),
	("PC_REC_INCR_XFER_SIZE", 20),
	("PC_REC_MAX_XFER_SIZE", 21),
	("PC_REC_MIN_XFER_SIZE", 22),
	("PC_REC_XFER_ALIGN", 23),
	("PC_SYMLINK_MAX", 24),
	("PC_SYNC_IO", 25),
	("PC_VDISABLE", 9),
];

const SYSCONF_NAMES: &[(&str, i32)] = &[
	("SC_2_CHAR_TERM", 20),
	("SC_2_C_BIND", 18),
	("SC_2_C_DEV", 19),
	("SC_2_FORT_DEV", 21),
	("SC_2_FORT_RUN", 22),
	("SC_2_LOCALEDEF", 23),
	("SC_2_SW_DEV", 24),
	("SC_2_UPE", 25),
	("SC_2_VERSION", 17),
	("SC_AIO_LISTIO_MAX", 42),
	("SC_AIO_MAX", 43),
	("SC_AIO_PRIO_DELTA_MAX", 44),
	("SC_ARG_MAX", 1),
	("SC_ASYNCHRONOUS_IO", 28),
	("SC_ATEXIT_MAX", 107),
	("SC_BC_BASE_MAX", 9),
	("SC_BC_DIM_MAX", 10),
	("SC_BC_SCALE_MAX", 11),
	("SC_BC_STRING_MAX", 12),
	("SC_CHILD_MAX", 2),
	("SC_CLK_TCK", 3),
	("SC_COLL_WEIGHTS_MAX", 13),
	("SC_DELAYTIMER_MAX", 45),
	("SC_EXPR_NEST_MAX", 14),
	("SC_FSYNC", 38),
	("SC_GETGR_R_SIZE_MAX", 70),
	("SC_GETPW_R_SIZE_MAX", 71),
	("SC_IOV_MAX", 56),
	("SC_JOB_CONTROL", 6),
	("SC_LINE_MAX", 15),
	("SC_LOGIN_NAME_MAX", 73),
	("SC_MAPPED_FILES", 47),
	("SC_MEMLOCK", 30),
	("SC_MEMLOCK_RANGE", 31),
	("SC_MEMORY_PROTECTION", 32),
	("SC_MESSAGE_PASSING", 33),
	("SC_MQ_OPEN_MAX", 46),
	("SC_MQ_PRIO_MAX", 75),
	("SC_NGROUPS_MAX", 4),
	("SC_NPROCESSORS_CONF", 57),
	("SC_NPROCESSORS_ONLN", 58),
	("SC_OPEN_MAX", 5),
	("SC_PAGESIZE", 29),
	("SC_PAGE_SIZE", 29),
	("SC_PASS_MAX", 131),
	("SC_PHYS_PAGES", 200),
	("SC_PRIORITIZED_IO", 34),
	("SC_PRIORITY_SCHEDULING", 35),
	("SC_REALTIME_SIGNALS", 36),
	("SC_RE_DUP_MAX", 16),
	("SC_RTSIG_MAX", 48),
	("SC_SAVED_IDS", 7),
	("SC_SEMAPHORES", 37),
	("SC_SEM_NSEMS_MAX", 49),
	("SC_SEM_VALUE_MAX", 50),
	("SC_SHARED_MEMORY_OBJECTS", 39),
	("SC_SIGQUEUE_MAX", 51),
	("SC_STREAM_MAX", 26),
	("SC_SYNCHRONIZED_IO", 40),
	("SC_THREADS", 96),
	("SC_THREAD_ATTR_STACKADDR", 82),
	("SC_THREAD_ATTR_STACKSIZE", 83),
	("SC_THREAD_DESTRUCTOR_ITERATIONS", 85),
	("SC_THREAD_KEYS_MAX", 86),
	("SC_THREAD_PRIORITY_SCHEDULING", 89),
	("SC_THREAD_PRIO_INHERIT", 87),
	("SC_THREAD_PRIO_PROTECT", 88),
	("SC_THREAD_PROCESS_SHARED", 90),
	("SC_THREAD_SAFE_FUNCTIONS", 91),
	("SC_THREAD_STACK_MIN", 93),
	("SC_THREAD_THREADS_MAX", 94),
	("SC_TIMERS", 41),
	("SC_TIMER_MAX", 52),
	("SC_TTY_NAME_MAX", 101),
	("SC_TZNAME_MAX", 27),
	("SC_VERSION", 8),
	("SC_XBS5_ILP32_OFF32", 122),
	("SC_XBS5_ILP32_OFFBIG", 123),
	("SC_XBS5_LP64_OFF64", 124),
	("SC_XBS5_LPBIG_OFFBIG", 125),
	("SC_XOPEN_CRYPT", 108),
	("SC_XOPEN_ENH_I18N", 109),
	("SC_XOPEN_LEGACY", 110),
	("SC_XOPEN_REALTIME", 111),
	("SC_XOPEN_REALTIME_THREADS", 112),
	("SC_XOPEN_SHM", 113),
	("SC_XOPEN_UNIX", 115),
	("SC_XOPEN_VERSION", 116),
	("SC_XOPEN_XCU_VERSION", 121),
];

/// `lseek(2)` whence constants served by the C `posix` module on the host
/// oracle: `SEEK_HOLE`/`SEEK_DATA` (sparse-file navigation) live on BOTH
/// `os` and `posix`, while the portable trio `SEEK_SET`/`SEEK_CUR`/
/// `SEEK_END` is defined by `os.py` itself and never re-exported into
/// `posix` — [`build_attrs`] adds the trio under its `module == "os"`
/// branch.  `zipfile` consumes `os.SEEK_SET`/`os.SEEK_CUR` at import time
/// (module-level `_EndRecData` helpers seed `whence` defaults), and
/// `importlib.metadata`/`pkgutil`/`zipimport` reach it through the zipfile
/// chain.  Values come from libc, so they always match the host CPython's
/// (darwin: HOLE=3/DATA=4; linux swaps them).
const SEEK_MODES: &[(&str, i32)] =
	&[("SEEK_DATA", libc::SEEK_DATA), ("SEEK_HOLE", libc::SEEK_HOLE)];

/// `os.py`-level `SEEK_SET`/`SEEK_CUR`/`SEEK_END` (see [`SEEK_MODES`]).
const SEEK_POSITIONS: &[(&str, i32)] =
	&[("SEEK_SET", libc::SEEK_SET), ("SEEK_CUR", libc::SEEK_CUR), ("SEEK_END", libc::SEEK_END)];

/// Live process-environment mapping used for `os.environ`/`posix.environ`.
///
/// The object is a dict-layout heap class so existing stdlib consumers keep the
/// normal dict surface (`get`, `keys`, iteration, `setdefault`, `pop`,
/// `update`, repr), while item assignment/deletion and the usual mutating
/// mapping methods update the real process environment before mutating mapping
/// storage.  Direct `putenv` and `unsetenv` calls also synchronize any cached
/// `os.environ`/`posix.environ` binding that still carries dict storage.
///
/// Remaining documented divergences: `repr(os.environ)` is the dict repr, not
/// CPython's `environ({...})`; `os.environb` is still a separate bytes snapshot;
/// `posix.environ` is str-keyed like `os.environ` rather than CPython's raw
/// bytes table; non-UTF-8 inherited entries are decoded lossily rather than
/// with CPython's `surrogateescape`; and explicit base-dict calls such as
/// `dict.__setitem__(os.environ, ...)` bypass write-through.
fn environ_mapping(module: &str) -> Result<*mut PyObject, String> {
	let class = environ_type()?;
	let namespace = crate::types::type_::new_namespace();
	let environ = crate::abi::map::alloc_dict_subclass_instance(
		class.cast::<crate::object::PyType>(),
		namespace,
		Vec::new(),
	)?;
	for (key, value) in std::env::vars_os() {
		let key = key.to_string_lossy();
		let value = value.to_string_lossy();
		// SAFETY: String allocation helpers copy the bytes; NULL is checked below.
		let key_obj = unsafe { pon_const_str(key.as_ptr(), key.len()) };
		let value_obj = unsafe { pon_const_str(value.as_ptr(), value.len()) };
		if key_obj.is_null() || value_obj.is_null() {
			return Err(format!("failed to allocate {module}.environ entry"));
		}
		// SAFETY: `environ` embeds dict storage and both objects are live strings.
		if unsafe { crate::abi::map::pon_dict_set_item_status(environ, key_obj, value_obj) } < 0 {
			return Err(format!("failed to populate {module}.environ"));
		}
	}
	Ok(environ)
}

fn environ_type() -> Result<*mut PyObject, String> {
	static TYPE: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
	let mut slot = TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
	if *slot != 0 {
		return Ok(*slot as *mut PyObject);
	}
	let type_type = crate::abi::runtime_type_type();
	if type_type.is_null() {
		return Err("runtime type type is not initialized".to_owned());
	}
	let dict_type = crate::types::dict::dict_type(type_type);
	let namespace = crate::types::type_::new_namespace();
	let natives: &[(&str, BuiltinFn)] = &[
		("__setitem__", environ_setitem_method),
		("__delitem__", environ_delitem_method),
		("setdefault", environ_setdefault_method),
		("pop", environ_pop_method),
		("popitem", environ_popitem_method),
		("clear", environ_clear_method),
		("update", environ_update_method),
	];
	for &(method, entry) in natives {
		let interned = intern(method);
		// SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
		let function =
			unsafe { crate::abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), interned) };
		if function.is_null() {
			return Err(format!("failed to allocate os._Environ.{method}"));
		}
		// SAFETY: Freshly built namespace box.
		unsafe { (&mut *namespace).set(interned, function) };
	}
	// SAFETY: The dict type is a live type object; the namespace was built above.
	let class = unsafe {
		crate::types::type_::build_class_from_namespace(
			"os._Environ",
			&[dict_type.cast::<PyObject>()],
			namespace,
			&[],
		)
	};
	if class.is_null() {
		crate::thread_state::pon_err_clear();
		return Err("failed to construct os._Environ".to_owned());
	}
	*slot = class as usize;
	Ok(class)
}

unsafe extern "C" fn environ_setitem_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "__setitem__") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let &[key, value] = args else {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.__setitem__ expected 2 arguments, got {}",
			args.len()
		));
	};
	if let Err(raised) = environ_store_pair(receiver, key, value) {
		return raised;
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn environ_delitem_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "__delitem__") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let &[key] = args else {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.__delitem__ expected 1 argument, got {}",
			args.len()
		));
	};
	if let Err(raised) = environ_remove_key(receiver, key, true).map(|_| ()) {
		return raised;
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn environ_setdefault_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "setdefault") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if !(1..=2).contains(&args.len()) {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.setdefault expected 1 or 2 arguments, got {}",
			args.len()
		));
	}
	let (_, key_obj) = match environ_key_object(args[0]) {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	match raw_environ_get(receiver, key_obj) {
		Ok(Some(existing)) => existing,
		Ok(None) => {
			let default = if let Some(&value) = args.get(1) {
				value
			} else {
				unsafe { crate::abi::pon_none() }
			};
			match environ_store_pair(receiver, args[0], default) {
				Ok(value) => value,
				Err(raised) => raised,
			}
		},
		Err(raised) => raised,
	}
}

unsafe extern "C" fn environ_pop_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "pop") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if !(1..=2).contains(&args.len()) {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.pop expected 1 or 2 arguments, got {}",
			args.len()
		));
	}
	match environ_remove_key(receiver, args[0], false) {
		Ok(Some(value)) => value,
		Ok(None) => {
			if let Some(&default) = args.get(1) {
				default
			} else {
				let (_, key_obj) = match environ_key_object(args[0]) {
					Ok(pair) => pair,
					Err(raised) => return raised,
				};
				unsafe { crate::abi::exc::pon_raise_key_error(key_obj) }
			}
		},
		Err(raised) => raised,
	}
}

unsafe extern "C" fn environ_popitem_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "popitem") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if !args.is_empty() {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.popitem expected 0 arguments, got {}",
			args.len()
		));
	}
	let entries = match raw_environ_entries(receiver) {
		Ok(entries) => entries,
		Err(raised) => return raised,
	};
	let Some(entry) = entries.last().copied() else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::KeyError,
			"popitem(): dictionary is empty",
		);
	};
	if let Err(raised) = environ_remove_key(receiver, entry.key, false) {
		return raised;
	}
	let mut items = [entry.key, entry.value];
	// SAFETY: `items` is a live two-object window for the duration of the call.
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn environ_clear_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "clear") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if !args.is_empty() {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.clear expected 0 arguments, got {}",
			args.len()
		));
	}
	let entries = match raw_environ_entries(receiver) {
		Ok(entries) => entries,
		Err(raised) => return raised,
	};
	for entry in entries {
		if let Err(raised) = environ_remove_key(receiver, entry.key, false).map(|_| ()) {
			return raised;
		}
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn environ_update_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { environ_method_args(argv, argc, "update") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let (args, kw_pairs) = match args.split_last() {
		Some((&last, rest)) => match unsafe { crate::types::lazy_iter::kw_marker_pairs(last) } {
			Some(pairs) => (rest, pairs),
			None => (args, &[][..]),
		},
		None => (args, &[][..]),
	};
	if args.len() > 1 {
		return crate::abi::return_null_with_error(format!(
			"os._Environ.update expected at most 1 argument, got {}",
			args.len()
		));
	}
	if let Some(&other) = args.first() {
		let mut pairs = Vec::new();
		// SAFETY: Pair collection follows the normal dict-update error contract.
		if unsafe { super::builtins_mod::collect_dict_update_pairs(other, &mut pairs) }.is_err() {
			return std::ptr::null_mut();
		}
		for pair in pairs.chunks_exact(2) {
			if let Err(raised) = environ_store_pair(receiver, pair[0], pair[1]) {
				return raised;
			}
		}
	}
	for &(name, value) in kw_pairs {
		let Some(text) = crate::intern::resolve(name) else {
			return crate::abi::return_null_with_error("os._Environ.update keyword name is not interned");
		};
		let key = unsafe { pon_const_str(text.as_ptr(), text.len()) };
		if key.is_null() {
			return std::ptr::null_mut();
		}
		if let Err(raised) = environ_store_pair(receiver, key, value) {
			return raised;
		}
	}
	unsafe { crate::abi::pon_none() }
}

fn environ_store_pair(
	receiver: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> Result<*mut PyObject, *mut PyObject> {
	let (name, key_obj) = environ_key_object(key)?;
	let value = environ_text_bytes_arg(value)?;
	env_set_bytes(&name, &value)?;
	let value_obj = env_object_from_bytes(&value, "os.environ value")?;
	raw_environ_set(receiver, key_obj, value_obj)?;
	sync_environ_bindings_set(key_obj, value_obj, receiver)?;
	Ok(value_obj)
}

fn environ_remove_key(
	receiver: *mut PyObject,
	key: *mut PyObject,
	raise_missing: bool,
) -> Result<Option<*mut PyObject>, *mut PyObject> {
	let (name, key_obj) = environ_key_object(key)?;
	let existing = raw_environ_get(receiver, key_obj)?;
	if existing.is_none() {
		if raise_missing {
			return Err(unsafe { crate::abi::exc::pon_raise_key_error(key_obj) });
		}
		return Ok(None);
	}
	env_unset_bytes(&name)?;
	raw_environ_unset(receiver, key_obj, false)?;
	sync_environ_bindings_unset(key_obj, receiver)?;
	Ok(existing)
}

fn environ_key_object(key: *mut PyObject) -> Result<(Vec<u8>, *mut PyObject), *mut PyObject> {
	let name = environ_text_bytes_arg(key)?;
	let key_obj = env_object_from_bytes(&name, "os.environ key")?;
	Ok((name, key_obj))
}

unsafe fn environ_method_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	method: &str,
) -> Result<(*mut PyObject, &'a [*mut PyObject]), *mut PyObject> {
	let args = unsafe { call_args(argv, argc) };
	let Some((&receiver, rest)) = args.split_first() else {
		return Err(crate::abi::return_null_with_error(format!(
			"os._Environ.{method} requires a receiver"
		)));
	};
	let receiver = crate::tag::untag_arg(receiver);
	if receiver.is_null() || unsafe { !crate::types::dict::is_dict_subclass_instance(receiver) } {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"descriptor requires an os._Environ object",
		));
	}
	Ok((receiver, rest))
}

fn environ_text_bytes_arg(object: *mut PyObject) -> Result<Vec<u8>, *mut PyObject> {
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() {
		return Err(std::ptr::null_mut());
	}
	if crate::tag::is_small_int(raw) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"str expected, not int",
		));
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	match unsafe { crate::types::type_::unicode_text(raw) } {
		Some(text) => Ok(text.as_bytes().to_vec()),
		None => {
			// SAFETY: Heap pointer with a live header after the tag checks.
			let display = unsafe { crate::types::dict::type_name(raw) }.unwrap_or("object");
			Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::TypeError,
				&format!("str expected, not {display}"),
			))
		},
	}
}

fn env_object_from_bytes(bytes: &[u8], what: &str) -> Result<*mut PyObject, *mut PyObject> {
	let text = String::from_utf8_lossy(bytes);
	// SAFETY: String allocation helper copies the bytes; NULL is checked below.
	let object = unsafe { pon_const_str(text.as_ptr(), text.len()) };
	if object.is_null() {
		return Err(crate::abi::return_null_with_error(format!("failed to allocate {what}")));
	}
	Ok(object)
}

fn raw_environ_set(
	environ: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> Result<(), *mut PyObject> {
	// SAFETY: The caller passes a dict-layout environment object and live key/value objects.
	if unsafe { crate::abi::map::pon_dict_set_item_status(environ, key, value) } < 0 {
		return Err(std::ptr::null_mut());
	}
	Ok(())
}

fn raw_environ_get(
	environ: *mut PyObject,
	key: *mut PyObject,
) -> Result<Option<*mut PyObject>, *mut PyObject> {
	let _guard = crate::sync::begin_critical_section(environ);
	// SAFETY: The caller passes a dict-layout environment object and a live key object.
	match unsafe { crate::types::dict::dict_get(environ, key) } {
		Ok(value) => Ok(value),
		Err(message) => Err(crate::abi::return_null_with_error(message)),
	}
}

fn raw_environ_entries(
	environ: *mut PyObject,
) -> Result<Vec<crate::types::dict::DictEntry>, *mut PyObject> {
	let _guard = crate::sync::begin_critical_section(environ);
	// SAFETY: The caller passes a dict-layout environment object.
	match unsafe { crate::types::dict::dict_entries_snapshot(environ) } {
		Ok(entries) => Ok(entries),
		Err(message) => Err(crate::abi::return_null_with_error(message)),
	}
}

fn raw_environ_unset(
	environ: *mut PyObject,
	key: *mut PyObject,
	raise_missing: bool,
) -> Result<(), *mut PyObject> {
	let _guard = crate::sync::begin_critical_section(environ);
	// SAFETY: The caller passes a dict-layout environment object and a live key object.
	match unsafe { crate::types::dict::dict_remove(environ, key) } {
		Ok(Some(_)) => Ok(()),
		Ok(None) if raise_missing => Err(unsafe { crate::abi::exc::pon_raise_key_error(key) }),
		Ok(None) => Ok(()),
		Err(message) => Err(crate::abi::return_null_with_error(message)),
	}
}

fn sync_environ_bindings_set(
	key: *mut PyObject,
	value: *mut PyObject,
	skip: *mut PyObject,
) -> Result<(), *mut PyObject> {
	for module in [intern("os"), intern("posix")] {
		let Some(environ) = crate::import::module_attr(module, intern("environ")) else {
			continue;
		};
		if environ == skip || unsafe { !crate::types::dict::has_dict_storage(environ) } {
			continue;
		}
		raw_environ_set(environ, key, value)?;
	}
	Ok(())
}

fn sync_environ_bindings_unset(
	key: *mut PyObject,
	skip: *mut PyObject,
) -> Result<(), *mut PyObject> {
	for module in [intern("os"), intern("posix")] {
		let Some(environ) = crate::import::module_attr(module, intern("environ")) else {
			continue;
		};
		if environ == skip || unsafe { !crate::types::dict::has_dict_storage(environ) } {
			continue;
		}
		raw_environ_unset(environ, key, false)?;
	}
	Ok(())
}

fn env_set_bytes(name: &[u8], value: &[u8]) -> Result<(), *mut PyObject> {
	if name.contains(&0) || value.contains(&0) {
		let message = "embedded null byte";
		// SAFETY: Typed raise helper.
		return Err(unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) });
	}
	if name.contains(&b'=') {
		let message = "illegal environment variable name";
		// SAFETY: Typed raise helper.
		return Err(unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) });
	}
	if name.is_empty() {
		// macOS setenv(3) rejects an empty name; CPython surfaces the errno.
		return Err(raise_errno(libc::EINVAL, None));
	}
	use std::os::unix::ffi::OsStrExt;
	// SAFETY: The `set_var` panic preconditions (empty name, '=', NUL) are
	// pre-checked above and raise Python errors instead; the remaining
	// concurrent-getenv data-race contract is setenv(3)'s own, which CPython's
	// process environment APIs share.
	unsafe {
		std::env::set_var(std::ffi::OsStr::from_bytes(name), std::ffi::OsStr::from_bytes(value));
	}
	Ok(())
}

fn env_unset_bytes(name: &[u8]) -> Result<(), *mut PyObject> {
	if name.contains(&0) {
		let message = "embedded null byte";
		// SAFETY: Typed raise helper.
		return Err(unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) });
	}
	if name.is_empty() || name.contains(&b'=') {
		// macOS unsetenv(3) rejects empty names and embedded '='; CPython surfaces the errno.
		return Err(raise_errno(libc::EINVAL, None));
	}
	use std::os::unix::ffi::OsStrExt;
	// SAFETY: The `remove_var` panic preconditions (empty name, '=', NUL) are
	// pre-checked above; the concurrent-getenv data-race contract is
	// unsetenv(3)'s own, which CPython shares.
	unsafe { std::env::remove_var(std::ffi::OsStr::from_bytes(name)) };
	Ok(())
}
fn environb_snapshot(module: &str) -> Result<*mut PyObject, String> {
	use std::os::unix::ffi::OsStrExt;

	let mut pairs: Vec<*mut PyObject> = Vec::new();
	for (key, value) in std::env::vars_os() {
		let key = key.as_os_str().as_bytes();
		let value = value.as_os_str().as_bytes();
		let key_obj = unsafe { crate::abi::str_::pon_const_bytes(key.as_ptr(), key.len()) };
		let value_obj = unsafe { crate::abi::str_::pon_const_bytes(value.as_ptr(), value.len()) };
		if key_obj.is_null() || value_obj.is_null() {
			return Err(format!("failed to allocate {module}.environb entry"));
		}
		pairs.push(key_obj);
		pairs.push(value_obj);
	}
	let pair_count = pairs.len() / 2;
	let environ = unsafe { crate::abi::map::pon_build_map(pairs.as_mut_ptr(), pair_count) };
	if environ.is_null() {
		return Err(format!("failed to allocate {module}.environb"));
	}
	Ok(environ)
}

// ---------------------------------------------------------------------------
// POSIX syscall surface: open/close/read/write/unlink/rmdir/lstat, the
// waitpid/wait-status family, plus the scandir frontier stub.  Raw libc
// calls over the same process fd space the `_io` native files wrap
// (`File::from_raw_fd`), with errno mapped onto CPython's OSError subclass
// hierarchy (PEP 3151).

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

static WALK_REGISTRY: crate::gcroot::RootRegistry = crate::gcroot::RootRegistry::new();
#[derive(Clone, Default)]
struct AtForkCallbacks {
	before:          Vec<usize>,
	after_in_child:  Vec<usize>,
	after_in_parent: Vec<usize>,
}

static AT_FORK_CALLBACKS: std::sync::LazyLock<std::sync::Mutex<AtForkCallbacks>> =
	std::sync::LazyLock::new(|| std::sync::Mutex::new(AtForkCallbacks::default()));

/// Python objects held by live `os.walk` iterators and registered fork hooks.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let mut roots = WALK_REGISTRY.held_roots();
	let callbacks = AT_FORK_CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	for callback in callbacks
		.before
		.iter()
		.chain(callbacks.after_in_child.iter())
		.chain(callbacks.after_in_parent.iter())
	{
		let object = *callback as *mut PyObject;
		if !object.is_null() && crate::tag::is_heap(object) {
			roots.push(object);
		}
	}
	roots
}

unsafe extern "C" fn os_register_at_fork(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error(
			"os.register_at_fork expected keyword-only before/after_in_child/after_in_parent",
		);
	}
	let before = match at_fork_callback_arg(args[0], "before") {
		Ok(callback) => callback,
		Err(error) => return error,
	};
	let after_in_child = match at_fork_callback_arg(args[1], "after_in_child") {
		Ok(callback) => callback,
		Err(error) => return error,
	};
	let after_in_parent = match at_fork_callback_arg(args[2], "after_in_parent") {
		Ok(callback) => callback,
		Err(error) => return error,
	};
	let mut callbacks = AT_FORK_CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(callback) = before {
		callbacks.before.push(callback as usize);
	}
	if let Some(callback) = after_in_child {
		callbacks.after_in_child.push(callback as usize);
	}
	if let Some(callback) = after_in_parent {
		callbacks.after_in_parent.push(callback as usize);
	}
	unsafe { crate::abi::pon_none() }
}

fn at_fork_callback_arg(
	value: *mut PyObject,
	name: &str,
) -> Result<Option<*mut PyObject>, *mut PyObject> {
	if is_none_value(value) {
		return Ok(None);
	}
	let callback = crate::tag::untag_arg(value);
	if crate::abi::call::is_callable_object(callback) {
		Ok(Some(callback))
	} else {
		Err(raise_type_error(&format!("os.register_at_fork() argument '{name}' must be callable")))
	}
}

fn at_fork_snapshot() -> AtForkCallbacks {
	AT_FORK_CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.clone()
}

fn call_at_fork_callbacks(callbacks: &[usize], reverse: bool) -> Result<(), *mut PyObject> {
	if reverse {
		for &callback in callbacks.iter().rev() {
			call_at_fork_callback(callback)?;
		}
	} else {
		for &callback in callbacks {
			call_at_fork_callback(callback)?;
		}
	}
	Ok(())
}

fn call_at_fork_callback(callback: usize) -> Result<(), *mut PyObject> {
	let result = unsafe { crate::abi::pon_call(callback as *mut PyObject, std::ptr::null_mut(), 0) };
	if result.is_null() {
		Err(std::ptr::null_mut())
	} else {
		Ok(())
	}
}

fn walk_symlinks_as_files() -> *mut PyObject {
	static SENTINEL: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		Box::into_raw(Box::new(crate::object::PyObjectHeader::new(runtime_object_type())))
			.cast::<PyObject>() as usize
	});
	*SENTINEL as *mut PyObject
}

fn runtime_object_type() -> *mut crate::object::PyType {
	crate::abi::runtime_global(intern("object"))
		.map_or(std::ptr::null_mut(), |object| object.cast::<crate::object::PyType>())
}

#[repr(C)]
struct PyScandirIterator {
	ob_base: crate::object::PyObjectHeader,
	path:    String,
	entries: Option<std::fs::ReadDir>,
}

#[repr(C)]
struct PyDirEntry {
	ob_base:       crate::object::PyObjectHeader,
	name:          String,
	path:          String,
	inode:         Option<i64>,
	stat_follow:   Option<CachedStat>,
	stat_nofollow: Option<CachedStat>,
}

#[derive(Clone, Copy)]
struct CachedStat {
	fields: StatFields,
	kind:   CachedFileKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CachedFileKind {
	Directory,
	File,
	Symlink,
	Other,
}

fn scandir_iterator_type() -> *mut crate::object::PyType {
	static SCANDIR_ITER_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"posix.ScandirIterator",
			std::mem::size_of::<PyScandirIterator>(),
		);
		ty.tp_iter = Some(scandir_iter);
		ty.tp_iternext = Some(scandir_next);
		ty.tp_getattro = Some(scandir_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*SCANDIR_ITER_TYPE as *mut crate::object::PyType
}

fn direntry_type() -> *mut crate::object::PyType {
	static DIRENTRY_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"posix.DirEntry",
			std::mem::size_of::<PyDirEntry>(),
		);
		ty.tp_getattro = Some(direntry_getattro);
		ty.tp_repr = Some(direntry_repr);
		ty.tp_str = Some(direntry_repr);
		Box::into_raw(Box::new(ty)) as usize
	});
	*DIRENTRY_TYPE as *mut crate::object::PyType
}

fn ensure_direntry_type_dict() -> Result<(), String> {
	let ty = direntry_type();
	if unsafe { !(*ty).tp_dict.is_null() } {
		return Ok(());
	}
	let namespace = crate::types::type_::new_namespace();
	if namespace.is_null() {
		return Err("failed to allocate os.DirEntry namespace".to_owned());
	}
	let function = unsafe {
		crate::abi::pon_make_function(
			direntry_fspath_method as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern("__fspath__"),
		)
	};
	if function.is_null() {
		return Err("failed to allocate os.DirEntry.__fspath__".to_owned());
	}
	unsafe {
		(*namespace).set(intern("__fspath__"), function);
		(*ty).tp_dict = namespace.cast::<PyObject>();
	}
	crate::sync::register_namespaced_type(ty);
	Ok(())
}

unsafe fn scandir_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyScandirIterator> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	if unsafe { (*object).ob_type } == scandir_iterator_type().cast_const() {
		Some(unsafe { &mut *object.cast::<PyScandirIterator>() })
	} else {
		None
	}
}

unsafe fn direntry_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyDirEntry> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	if unsafe { (*object).ob_type } == direntry_type().cast_const() {
		Some(unsafe { &mut *object.cast::<PyDirEntry>() })
	} else {
		None
	}
}

fn alloc_scandir_iterator(path: String, entries: std::fs::ReadDir) -> *mut PyObject {
	Box::into_raw(Box::new(PyScandirIterator {
		ob_base: crate::object::PyObjectHeader::new(scandir_iterator_type()),
		path,
		entries: Some(entries),
	}))
	.cast::<PyObject>()
}

fn alloc_direntry(parent: &str, entry: std::fs::DirEntry) -> *mut PyObject {
	let name = entry.file_name().to_string_lossy().into_owned();
	let path = walk_join(parent, &name);
	Box::into_raw(Box::new(PyDirEntry {
		ob_base: crate::object::PyObjectHeader::new(direntry_type()),
		name,
		path,
		inode: direntry_inode(&entry),
		stat_follow: None,
		stat_nofollow: None,
	}))
	.cast::<PyObject>()
}

fn direntry_inode(entry: &std::fs::DirEntry) -> Option<i64> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::DirEntryExt;
		Some(stat_i64(entry.ino()))
	}
	#[cfg(not(unix))]
	{
		let _ = entry;
		None
	}
}

unsafe extern "C" fn scandir_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn scandir_next(object: *mut PyObject) -> *mut PyObject {
	let Some(iterator) = (unsafe { scandir_receiver(object) }) else {
		return raise_type_error("os.scandir iterator receiver is invalid");
	};
	let Some(entries) = iterator.entries.as_mut() else {
		return unsafe { crate::abi::exc::pon_raise_stop_iteration(std::ptr::null_mut()) };
	};
	match entries.next() {
		Some(Ok(entry)) => alloc_direntry(&iterator.path, entry),
		Some(Err(error)) => {
			raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&iterator.path))
		},
		None => {
			iterator.entries = None;
			unsafe { crate::abi::exc::pon_raise_stop_iteration(std::ptr::null_mut()) }
		},
	}
}

unsafe extern "C" fn scandir_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_type_error("attribute name must be str");
	};
	match name_text {
		"close" => bound_os_method(object, "close", scandir_close_method),
		"__enter__" => bound_os_method(object, "__enter__", scandir_enter_method),
		"__exit__" => bound_os_method(object, "__exit__", scandir_exit_method),
		"__iter__" => bound_os_method(object, "__iter__", scandir_iter_method),
		"__next__" => bound_os_method(object, "__next__", scandir_next_method),
		_ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn direntry_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_type_error("attribute name must be str");
	};
	let Some(entry) = (unsafe { direntry_receiver(object) }) else {
		return raise_type_error("os.DirEntry receiver is invalid");
	};
	match name_text {
		"name" => walk_str(&entry.name),
		"path" => walk_str(&entry.path),
		"inode" => bound_os_method(object, "inode", direntry_inode_method),
		"is_dir" => bound_direntry_follow_method(object, "is_dir", direntry_is_dir_method),
		"is_file" => bound_direntry_follow_method(object, "is_file", direntry_is_file_method),
		"is_symlink" => bound_os_method(object, "is_symlink", direntry_is_symlink_method),
		"stat" => bound_direntry_follow_method(object, "stat", direntry_stat_method),
		"__fspath__" => bound_os_method(object, "__fspath__", direntry_fspath_method),
		"__repr__" => bound_os_method(object, "__repr__", direntry_repr_method),
		_ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

fn bound_os_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	let function = unsafe {
		crate::abi::pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern(name),
		)
	};
	if function.is_null() {
		return std::ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => crate::abi::return_null_with_error(message),
	}
}

fn bound_direntry_follow_method(
	receiver: *mut PyObject,
	name: &str,
	entry: BuiltinFn,
) -> *mut PyObject {
	let default = bool_object(true);
	if default.is_null() {
		return std::ptr::null_mut();
	}
	let names = [intern("self"), intern("follow_symlinks")];
	let params = ParamSpec {
		names:                 names.as_ptr(),
		total_param_count:     names.len() as u32,
		positional_only_count: 0,
		positional_count:      1,
		keyword_only_count:    1,
		varargs_name:          0,
		varkw_name:            0,
	};
	let code = CodeInfo {
		entry:         entry as *const u8,
		params:        &params,
		name_interned: intern(name),
		n_locals:      0,
		n_feedback:    0,
		flags:         0,
	};
	let kwdefault_names = [intern("follow_symlinks")];
	let mut kwdefaults = [default];
	let function = unsafe {
		crate::abi::call::pon_make_function_full(
			&code,
			std::ptr::null_mut(),
			0,
			kwdefault_names.as_ptr(),
			kwdefaults.as_mut_ptr(),
			kwdefaults.len(),
			std::ptr::null(),
			std::ptr::null_mut(),
			0,
		)
	};
	if function.is_null() {
		return std::ptr::null_mut();
	}
	crate::types::function::mark_native_function(function);
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => crate::abi::return_null_with_error(message),
	}
}

unsafe fn method_arg_slice<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
	if argv.is_null() && argc != 0 {
		return Err(raise_type_error(&format!("{name}() argv pointer is null")));
	}
	let args = if argc == 0 {
		&[]
	} else {
		unsafe { std::slice::from_raw_parts(argv, argc) }
	};
	if args.is_empty() {
		return Err(raise_type_error(&format!("{name}() missing receiver")));
	}
	Ok(args)
}

unsafe fn scandir_method_receiver<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<(&'a mut PyScandirIterator, &'a [*mut PyObject]), *mut PyObject> {
	let args = unsafe { method_arg_slice(argv, argc, name) }?;
	let Some(iterator) = (unsafe { scandir_receiver(args[0]) }) else {
		return Err(raise_type_error(&format!("{name}() receiver is not a ScandirIterator")));
	};
	Ok((iterator, &args[1..]))
}

unsafe fn direntry_method_receiver<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<(&'a mut PyDirEntry, &'a [*mut PyObject]), *mut PyObject> {
	let args = unsafe { method_arg_slice(argv, argc, name) }?;
	let Some(entry) = (unsafe { direntry_receiver(args[0]) }) else {
		return Err(raise_type_error(&format!("{name}() receiver is not a DirEntry")));
	};
	Ok((entry, &args[1..]))
}

unsafe extern "C" fn scandir_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (iterator, args) = match unsafe { scandir_method_receiver(argv, argc, "close") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise_type_error("close() takes no arguments");
	}
	iterator.entries = None;
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn scandir_enter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { method_arg_slice(argv, argc, "__enter__") } {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return raise_type_error("__enter__() takes no arguments");
	}
	if unsafe { scandir_receiver(args[0]) }.is_none() {
		return raise_type_error("__enter__() receiver is not a ScandirIterator");
	}
	args[0]
}

unsafe extern "C" fn scandir_exit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (iterator, args) = match unsafe { scandir_method_receiver(argv, argc, "__exit__") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if args.len() > 3 {
		return raise_type_error("__exit__() expected at most 3 arguments");
	}
	iterator.entries = None;
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn scandir_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { method_arg_slice(argv, argc, "__iter__") } {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return raise_type_error("__iter__() takes no arguments");
	}
	unsafe { scandir_iter(args[0]) }
}

unsafe extern "C" fn scandir_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { method_arg_slice(argv, argc, "__next__") } {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return raise_type_error("__next__() takes no arguments");
	}
	unsafe { scandir_next(args[0]) }
}

impl PyDirEntry {
	fn cached_stat(&mut self, follow_symlinks: bool) -> Result<CachedStat, std::io::Error> {
		let slot = if follow_symlinks {
			&mut self.stat_follow
		} else {
			&mut self.stat_nofollow
		};
		if let Some(cached) = *slot {
			return Ok(cached);
		}
		let metadata = if follow_symlinks {
			std::fs::metadata(&self.path)
		} else {
			std::fs::symlink_metadata(&self.path)
		}?;
		let cached = cached_stat_from_metadata(&metadata);
		*slot = Some(cached);
		Ok(cached)
	}
}

fn cached_stat_from_metadata(metadata: &std::fs::Metadata) -> CachedStat {
	let file_type = metadata.file_type();
	let kind = if file_type.is_dir() {
		CachedFileKind::Directory
	} else if file_type.is_file() {
		CachedFileKind::File
	} else if file_type.is_symlink() {
		CachedFileKind::Symlink
	} else {
		CachedFileKind::Other
	};
	CachedStat { fields: stat_fields_from_metadata(metadata), kind }
}

fn follow_symlinks_arg(args: &[*mut PyObject]) -> Result<bool, *mut PyObject> {
	match args {
		[] => Ok(true),
		[value] => truth_arg(*value),
		_ => Err(raise_type_error("follow_symlinks is a keyword-only argument")),
	}
}

fn is_missing_entry_error(error: &std::io::Error) -> bool {
	matches!(error.raw_os_error(), Some(errno) if errno == libc::ENOENT || errno == libc::ENOTDIR)
}

unsafe extern "C" fn direntry_inode_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (entry, args) = match unsafe { direntry_method_receiver(argv, argc, "inode") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise_type_error("inode() takes no arguments");
	}
	let inode = match entry.inode {
		Some(inode) => inode,
		None => match entry.cached_stat(false) {
			Ok(stat) => stat.fields.st_ino,
			Err(error) => {
				return raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&entry.path));
			},
		},
	};
	unsafe { crate::abi::pon_const_int(inode) }
}

unsafe extern "C" fn direntry_is_dir_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	direntry_kind_predicate(argv, argc, "is_dir", CachedFileKind::Directory)
}

unsafe extern "C" fn direntry_is_file_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	direntry_kind_predicate(argv, argc, "is_file", CachedFileKind::File)
}

fn direntry_kind_predicate(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	expected: CachedFileKind,
) -> *mut PyObject {
	let (entry, args) = match unsafe { direntry_method_receiver(argv, argc, name) } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	let follow_symlinks = match follow_symlinks_arg(args) {
		Ok(value) => value,
		Err(error) => return error,
	};
	match entry.cached_stat(follow_symlinks) {
		Ok(stat) => bool_object(stat.kind == expected),
		Err(error) if is_missing_entry_error(&error) => bool_object(false),
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&entry.path)),
	}
}

unsafe extern "C" fn direntry_is_symlink_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (entry, args) = match unsafe { direntry_method_receiver(argv, argc, "is_symlink") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise_type_error("is_symlink() takes no arguments");
	}
	match entry.cached_stat(false) {
		Ok(stat) => bool_object(stat.kind == CachedFileKind::Symlink),
		Err(error) if is_missing_entry_error(&error) => bool_object(false),
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&entry.path)),
	}
}

unsafe extern "C" fn direntry_stat_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (entry, args) = match unsafe { direntry_method_receiver(argv, argc, "stat") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	let follow_symlinks = match follow_symlinks_arg(args) {
		Ok(value) => value,
		Err(error) => return error,
	};
	match entry.cached_stat(follow_symlinks) {
		Ok(stat) => stat_result_from_fields(stat.fields),
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&entry.path)),
	}
}

unsafe extern "C" fn direntry_fspath_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (entry, args) = match unsafe { direntry_method_receiver(argv, argc, "__fspath__") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise_type_error("__fspath__() takes no arguments");
	}
	walk_str(&entry.path)
}

unsafe extern "C" fn direntry_repr_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { method_arg_slice(argv, argc, "__repr__") } {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return raise_type_error("__repr__() takes no arguments");
	}
	unsafe { direntry_repr(args[0]) }
}

unsafe extern "C" fn direntry_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(entry) = (unsafe { direntry_receiver(object) }) else {
		return raise_type_error("os.DirEntry receiver is invalid");
	};
	let text = format!("<DirEntry '{}'>", entry.name);
	walk_str(&text)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

#[repr(C)]
struct PyWalk {
	ob_base:           crate::object::PyObjectHeader,
	stack:             Vec<WalkStackEntry>,
	pending_topdown:   Option<PendingTopDown>,
	topdown:           bool,
	followlinks:       bool,
	symlinks_as_files: bool,
	onerror:           *mut PyObject,
}

enum WalkStackEntry {
	Path(String),
	Yield { top: String, dirs: Vec<String>, files: Vec<String> },
}

struct PendingTopDown {
	top:      String,
	dirnames: *mut PyObject,
}

struct WalkScan {
	dirs:      Vec<String>,
	files:     Vec<String>,
	walk_dirs: Vec<String>,
}

struct WalkIoError {
	errno: i32,
	path:  String,
}

impl crate::gcroot::HeldRoots for PyWalk {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.onerror);
		if let Some(pending) = &self.pending_topdown {
			push(pending.dirnames);
		}
	}
}

fn walk_type() -> *mut crate::object::PyType {
	static WALK_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"os.walk",
			std::mem::size_of::<PyWalk>(),
		);
		ty.tp_iter = Some(walk_iter);
		ty.tp_iternext = Some(walk_next);
		ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*WALK_TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn walk_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe fn walk_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyWalk> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	if unsafe { (*object).ob_type } == walk_type().cast_const() {
		Some(unsafe { &mut *object.cast::<PyWalk>() })
	} else {
		None
	}
}

fn alloc_walk(
	top: String,
	topdown: bool,
	onerror: *mut PyObject,
	followlinks: bool,
	symlinks_as_files: bool,
) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyWalk {
		ob_base: crate::object::PyObjectHeader::new(walk_type()),
		stack: vec![WalkStackEntry::Path(top)],
		pending_topdown: None,
		topdown,
		followlinks,
		symlinks_as_files,
		onerror,
	}))
	.cast::<PyObject>();
	WALK_REGISTRY.register::<PyWalk>(object)
}

/// `os.walk(top, topdown=True, onerror=None, followlinks=False)`.
unsafe extern "C" fn os_walk(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 4 {
		return crate::abi::return_null_with_error("os.walk expected 1 to 4 arguments");
	}
	let top = match path_arg(args[0], "walk") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let topdown = match truth_arg(args[1]) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let onerror = if is_none_value(args[2]) {
		std::ptr::null_mut()
	} else {
		args[2]
	};
	let follow_arg = crate::tag::untag_arg(args[3]);
	let (followlinks, symlinks_as_files) = if follow_arg == walk_symlinks_as_files() {
		(false, true)
	} else {
		match truth_arg(args[3]) {
			Ok(value) => (value, false),
			Err(error) => return error,
		}
	};
	alloc_walk(top, topdown, onerror, followlinks, symlinks_as_files)
}

fn truth_arg(object: *mut PyObject) -> Result<bool, *mut PyObject> {
	match unsafe { crate::abi::pon_is_true(object) } {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(std::ptr::null_mut()),
	}
}

unsafe extern "C" fn walk_next(object: *mut PyObject) -> *mut PyObject {
	let Some(walk) = (unsafe { walk_receiver(object) }) else {
		return crate::abi::return_null_with_error("os.walk iterator receiver is invalid");
	};
	if let Err(error) = enqueue_pending_topdown(walk) {
		return error;
	}
	loop {
		let Some(entry) = walk.stack.pop() else {
			return unsafe { crate::abi::exc::pon_raise_stop_iteration(std::ptr::null_mut()) };
		};
		match entry {
			WalkStackEntry::Yield { top, dirs, files } => return build_walk_tuple(&top, dirs, files),
			WalkStackEntry::Path(top) => {
				let scan =
					match scan_walk_dir(&top, walk.topdown, walk.followlinks, walk.symlinks_as_files) {
						Ok(scan) => scan,
						Err(error) => {
							if let Err(raised) = call_walk_onerror(walk, error) {
								return raised;
							}
							continue;
						},
					};
				if walk.topdown {
					return yield_topdown(walk, top, scan);
				}
				walk
					.stack
					.push(WalkStackEntry::Yield { top, dirs: scan.dirs, files: scan.files });
				for child in scan.walk_dirs.into_iter().rev() {
					walk.stack.push(WalkStackEntry::Path(child));
				}
			},
		}
	}
}

fn enqueue_pending_topdown(walk: &mut PyWalk) -> Result<(), *mut PyObject> {
	let Some(pending) = walk.pending_topdown.take() else {
		return Ok(());
	};
	let names = match crate::abi::seq::sequence_to_vec(pending.dirnames) {
		Ok(names) => names,
		Err(message) => return Err(crate::abi::return_null_with_error(message)),
	};
	for name_object in names.into_iter().rev() {
		let name = match path_arg(name_object, "walk") {
			Ok(name) => name,
			Err(error) => return Err(error),
		};
		let child = walk_join(&pending.top, &name);
		if walk.followlinks || walk.symlinks_as_files || !path_is_symlink(&child) {
			walk.stack.push(WalkStackEntry::Path(child));
		}
	}
	Ok(())
}

fn yield_topdown(walk: &mut PyWalk, top: String, scan: WalkScan) -> *mut PyObject {
	let top_object = walk_str(&top);
	let dirnames = super::builtins_batch::build_str_list(scan.dirs);
	let filenames = super::builtins_batch::build_str_list(scan.files);
	if top_object.is_null() || dirnames.is_null() || filenames.is_null() {
		return std::ptr::null_mut();
	}
	walk.pending_topdown = Some(PendingTopDown { top, dirnames });
	build_walk_tuple_objects(top_object, dirnames, filenames)
}

fn build_walk_tuple(top: &str, dirs: Vec<String>, files: Vec<String>) -> *mut PyObject {
	let top_object = walk_str(top);
	let dirnames = super::builtins_batch::build_str_list(dirs);
	let filenames = super::builtins_batch::build_str_list(files);
	build_walk_tuple_objects(top_object, dirnames, filenames)
}

fn build_walk_tuple_objects(
	top: *mut PyObject,
	dirs: *mut PyObject,
	files: *mut PyObject,
) -> *mut PyObject {
	if top.is_null() || dirs.is_null() || files.is_null() {
		return std::ptr::null_mut();
	}
	let mut items = [top, dirs, files];
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn walk_str(text: &str) -> *mut PyObject {
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

fn scan_walk_dir(
	top: &str,
	topdown: bool,
	followlinks: bool,
	symlinks_as_files: bool,
) -> Result<WalkScan, WalkIoError> {
	let entries = std::fs::read_dir(top).map_err(|error| walk_io_error(error, top))?;
	let mut dirs = Vec::new();
	let mut files = Vec::new();
	let mut walk_dirs = Vec::new();
	for entry in entries {
		let entry = entry.map_err(|error| walk_io_error(error, top))?;
		let file_type = entry.file_type();
		let is_dir = match &file_type {
			Ok(file_type) if file_type.is_dir() => true,
			Ok(file_type) if file_type.is_symlink() && !symlinks_as_files => {
				std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir())
			},
			Ok(_) | Err(_) => false,
		};
		let name = entry.file_name().to_string_lossy().into_owned();
		if is_dir {
			dirs.push(name);
			if !topdown {
				let is_symlink = file_type
					.as_ref()
					.is_ok_and(|file_type| file_type.is_symlink());
				if followlinks || !is_symlink {
					walk_dirs.push(entry.path().to_string_lossy().into_owned());
				}
			}
		} else {
			files.push(name);
		}
	}
	Ok(WalkScan { dirs, files, walk_dirs })
}

fn walk_io_error(error: std::io::Error, path: &str) -> WalkIoError {
	WalkIoError { errno: error.raw_os_error().unwrap_or(libc::EIO), path: path.to_owned() }
}

fn call_walk_onerror(walk: &PyWalk, error: WalkIoError) -> Result<(), *mut PyObject> {
	if walk.onerror.is_null() {
		return Ok(());
	}
	let exception = match alloc_errno_exception(error.errno, Some(&error.path)) {
		Ok(exception) => exception,
		Err(raised) => return Err(raised),
	};
	let mut args = [exception];
	let result = unsafe { crate::abi::pon_call(walk.onerror, args.as_mut_ptr(), args.len()) };

	if result.is_null() {
		Err(std::ptr::null_mut())
	} else {
		Ok(())
	}
}

/// `os.listdir(path='.')`: names in directory iteration order, excluding
/// the synthetic `.` and `..` entries.
unsafe extern "C" fn os_listdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() > 1 {
		return crate::abi::return_null_with_error("os.listdir expected at most one argument");
	}
	let path = if args.first().copied().is_none_or(is_none_value) {
		".".to_owned()
	} else {
		match path_arg(args[0], "listdir") {
			Ok(path) => path,
			Err(error) => return error,
		}
	};
	if let Err(error) = c_path(&path) {
		return error;
	}
	let entries = match std::fs::read_dir(&path) {
		Ok(entries) => entries,
		Err(error) => return raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
	};
	let mut names = Vec::new();
	for entry in entries {
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => return raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
		};
		let name = entry.file_name().to_string_lossy().into_owned();
		if name == "." || name == ".." {
			continue;
		}
		let object = unsafe { pon_const_str(name.as_ptr(), name.len()) };
		if object.is_null() {
			return std::ptr::null_mut();
		}
		names.push(object);
	}
	unsafe { crate::abi::seq::pon_build_list(names.as_mut_ptr(), names.len()) }
}

fn walk_join(top: &str, name: &str) -> String {
	if name.starts_with('/') {
		name.to_owned()
	} else if top.is_empty() || top.ends_with('/') {
		format!("{top}{name}")
	} else {
		format!("{top}/{name}")
	}
}

fn path_is_symlink(path: &str) -> bool {
	std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// Name / entry / arity rows consumed by [`build_attrs`].  `fdopen`, `dup2`,
/// `open`, and `lstat` are variadic: `fdopen`/`dup2` have optional trailing
/// positionals, `open` has optional `mode`, and path functions may accept a
/// keyword-only `dir_fd` that the native keyword binder flattens into a
/// trailing positional None slot.
const SYSCALL_FUNCTIONS: &[(&str, BuiltinFn, usize)] = &[
	("WEXITSTATUS", os_wexitstatus, 1),
	("WIFCONTINUED", os_wifcontinued, 1),
	("WIFEXITED", os_wifexited, 1),
	("WIFSIGNALED", os_wifsignaled, 1),
	("WIFSTOPPED", os_wifstopped, 1),
	("WSTOPSIG", os_wstopsig, 1),
	("WTERMSIG", os_wtermsig, 1),
	("_exit", os_exit, 1),
	("abort", os_abort, 0),
	("access", os_access, 2),
	("chdir", os_chdir, 1),
	#[cfg(target_os = "macos")]
	("chflags", os_chflags, 2),
	("chown", os_chown, 3),
	("chroot", os_chroot, 1),
	("close", os_close, 1),
	("closerange", os_closerange, 2),
	("cpu_count", os_cpu_count, 0),
	("ctermid", os_ctermid, 0),
	("dup", os_dup, 1),
	("dup2", os_dup2, crate::native::builtins_mod::VARIADIC_ARITY),
	("fchdir", os_fchdir, 1),
	("fchmod", os_fchmod, 2),
	("fchown", os_fchown, 3),
	("fdopen", os_fdopen, crate::native::builtins_mod::VARIADIC_ARITY),
	("fstat", os_fstat, 1),
	("fstatvfs", os_fstatvfs, 1),
	("fork", os_fork, 0),
	("forkpty", os_forkpty, 0),
	("fsync", os_fsync, 1),
	("ftruncate", os_ftruncate, 2),
	("get_blocking", os_get_blocking, 1),
	("get_inheritable", os_get_inheritable, 1),
	("getcwd", os_getcwd, 0),
	("getcwdb", os_getcwdb, 0),
	("getegid", os_getegid, 0),
	("geteuid", os_geteuid, 0),
	("getgid", os_getgid, 0),
	("getgroups", os_getgroups, 0),
	("getgrouplist", os_getgrouplist, 2),
	("getloadavg", os_getloadavg, 0),
	("getlogin", os_getlogin, 0),
	("getpgid", os_getpgid, 1),
	("getpgrp", os_getpgrp, 0),
	("getpid", os_getpid, 0),
	("getppid", os_getppid, 0),
	("getpriority", os_getpriority, 2),
	("getsid", os_getsid, 1),
	("grantpt", os_grantpt, 1),
	("initgroups", os_initgroups, 2),
	("getuid", os_getuid, 0),
	("isatty", os_isatty, 1),
	("kill", os_kill, 2),
	("killpg", os_killpg, 2),
	#[cfg(target_os = "macos")]
	("lchflags", os_lchflags, 2),
	#[cfg(target_os = "macos")]
	("lchmod", os_lchmod, 2),
	("lchown", os_lchown, 3),
	("link", os_link, crate::native::builtins_mod::VARIADIC_ARITY),
	("lseek", os_lseek, 3),
	("execv", os_execv, 2),
	("execve", os_execve, 3),
	("lstat", os_lstat, crate::native::builtins_mod::VARIADIC_ARITY),
	("major", os_major, 1),
	("makedev", os_makedev, 2),
	("minor", os_minor, 1),
	("mkdir", os_mkdir, crate::native::builtins_mod::VARIADIC_ARITY),
	("mkfifo", os_mkfifo, crate::native::builtins_mod::VARIADIC_ARITY),
	("nice", os_nice, 1),
	("openpty", os_openpty, 0),
	("open", os_open, crate::native::builtins_mod::VARIADIC_ARITY),
	("pipe", os_pipe, 0),
	("posix_openpt", os_posix_openpt, 1),
	("posix_spawn", os_posix_spawn, crate::native::builtins_mod::VARIADIC_ARITY),
	("posix_spawnp", os_posix_spawnp, crate::native::builtins_mod::VARIADIC_ARITY),
	("pread", os_pread, 3),
	("putenv", os_putenv, 2),
	("pwrite", os_pwrite, 3),
	("preadv", os_preadv, crate::native::builtins_mod::VARIADIC_ARITY),
	("pwritev", os_pwritev, crate::native::builtins_mod::VARIADIC_ARITY),
	("ptsname", os_ptsname, 1),
	("read", os_read, 2),
	("readinto", os_readinto, 2),
	("readv", os_readv, 2),
	("readlink", os_readlink, 1),
	("rename", os_rename, crate::native::builtins_mod::VARIADIC_ARITY),
	("replace", os_replace, crate::native::builtins_mod::VARIADIC_ARITY),
	("remove", os_unlink, 1),
	("rmdir", os_rmdir, 1),
	("sched_get_priority_max", os_sched_get_priority_max, 1),
	("sched_get_priority_min", os_sched_get_priority_min, 1),
	("sched_yield", os_sched_yield, 0),
	("sendfile", os_sendfile, crate::native::builtins_mod::VARIADIC_ARITY),
	("set_blocking", os_set_blocking, 2),
	("set_inheritable", os_set_inheritable, 2),
	("setegid", os_setegid, 1),
	("seteuid", os_seteuid, 1),
	("setgid", os_setgid, 1),
	("setgroups", os_setgroups, 1),
	("setpgid", os_setpgid, 2),
	("setpgrp", os_setpgrp, 0),
	("setpriority", os_setpriority, 3),
	("setregid", os_setregid, 2),
	("setreuid", os_setreuid, 2),
	("setsid", os_setsid, 0),
	("setuid", os_setuid, 1),
	("strerror", os_strerror, 1),
	("symlink", os_symlink, crate::native::builtins_mod::VARIADIC_ARITY),
	("sync", os_sync, 0),
	("system", os_system, 1),
	("tcgetpgrp", os_tcgetpgrp, 1),
	("tcsetpgrp", os_tcsetpgrp, 2),
	("times", os_times, 0),
	("statvfs", os_statvfs, 1),
	("truncate", os_truncate, 2),
	("ttyname", os_ttyname, 1),
	("umask", os_umask, 1),
	("uname", os_uname, 0),
	("utime", os_utime, crate::native::builtins_mod::VARIADIC_ARITY),
	("wait3", os_wait3, 1),
	("wait4", os_wait4, 2),
	("waitid", os_waitid, 3),
	("unlink", os_unlink, 1),
	("unlockpt", os_unlockpt, 1),
	("unsetenv", os_unsetenv, 1),
	("wait", os_wait, 0),
	("waitpid", os_waitpid, 2),
	("waitstatus_to_exitcode", os_waitstatus_to_exitcode, 1),
	("write", os_write, 2),
	("writev", os_writev, 2),
];

const STATVFS_FIELDS: [&str; 10] = [
	"f_bsize",
	"f_frsize",
	"f_blocks",
	"f_bfree",
	"f_bavail",
	"f_files",
	"f_ffree",
	"f_favail",
	"f_flag",
	"f_namemax",
];

#[repr(C)]
struct PyStatVfsResult {
	ob_base: crate::object::PyObjectHeader,
	values:  [i64; 10],
}

static STATVFS_SEQUENCE: std::sync::LazyLock<crate::object::PySequenceMethods> =
	std::sync::LazyLock::new(|| crate::object::PySequenceMethods {
		sq_length: Some(statvfs_result_len),
		sq_item: Some(statvfs_result_item),
		..crate::object::PySequenceMethods::EMPTY
	});

fn statvfs_result_type() -> *mut crate::object::PyType {
	static TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"os.statvfs_result",
			std::mem::size_of::<PyStatVfsResult>(),
		);
		ty.tp_as_sequence = &*STATVFS_SEQUENCE as *const crate::object::PySequenceMethods
			as *mut crate::object::PySequenceMethods;
		ty.tp_getattro = Some(statvfs_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn statvfs_result_len(_object: *mut PyObject) -> isize {
	STATVFS_FIELDS.len() as isize
}

unsafe extern "C" fn statvfs_result_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	if index < 0 || index as usize >= STATVFS_FIELDS.len() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"statvfs_result index out of range",
		);
	}
	let result = object.cast::<PyStatVfsResult>();
	unsafe { crate::abi::pon_const_int((*result).values[index as usize]) }
}

unsafe extern "C" fn statvfs_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if let Some(index) = STATVFS_FIELDS.iter().position(|field| *field == name_text) {
		return unsafe { statvfs_result_item(object, index as isize) };
	}
	unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

fn statvfs_values(raw: &libc::statvfs) -> [i64; 10] {
	[
		stat_i64(raw.f_bsize),
		stat_i64(raw.f_frsize),
		stat_i64(raw.f_blocks),
		stat_i64(raw.f_bfree),
		stat_i64(raw.f_bavail),
		stat_i64(raw.f_files),
		stat_i64(raw.f_ffree),
		stat_i64(raw.f_favail),
		stat_i64(raw.f_flag),
		stat_i64(raw.f_namemax),
	]
}

fn statvfs_result_object(raw: &libc::statvfs) -> *mut PyObject {
	Box::into_raw(Box::new(PyStatVfsResult {
		ob_base: crate::object::PyObjectHeader::new(statvfs_result_type()),
		values:  statvfs_values(raw),
	}))
	.cast::<PyObject>()
}

const TIMES_FIELDS: [&str; 5] = ["user", "system", "children_user", "children_system", "elapsed"];

#[repr(C)]
struct PyTimesResult {
	ob_base: crate::object::PyObjectHeader,
	values:  [f64; 5],
}

static TIMES_SEQUENCE: std::sync::LazyLock<crate::object::PySequenceMethods> =
	std::sync::LazyLock::new(|| crate::object::PySequenceMethods {
		sq_length: Some(times_result_len),
		sq_item: Some(times_result_item),
		..crate::object::PySequenceMethods::EMPTY
	});

fn times_result_type() -> *mut crate::object::PyType {
	static TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"posix.times_result",
			std::mem::size_of::<PyTimesResult>(),
		);
		ty.tp_as_sequence = &*TIMES_SEQUENCE as *const crate::object::PySequenceMethods
			as *mut crate::object::PySequenceMethods;
		ty.tp_getattro = Some(times_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn times_result_len(_object: *mut PyObject) -> isize {
	TIMES_FIELDS.len() as isize
}

unsafe extern "C" fn times_result_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	if index < 0 || index as usize >= TIMES_FIELDS.len() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"times_result index out of range",
		);
	}
	let result = object.cast::<PyTimesResult>();
	unsafe { crate::abi::number::pon_const_float((*result).values[index as usize]) }
}

unsafe extern "C" fn times_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if let Some(index) = TIMES_FIELDS.iter().position(|field| *field == name_text) {
		return unsafe { times_result_item(object, index as isize) };
	}
	unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

fn times_result_object(values: [f64; 5]) -> *mut PyObject {
	Box::into_raw(Box::new(PyTimesResult {
		ob_base: crate::object::PyObjectHeader::new(times_result_type()),
		values,
	}))
	.cast::<PyObject>()
}

const UNAME_FIELDS: [&str; 5] = ["sysname", "nodename", "release", "version", "machine"];

#[repr(C)]
struct PyUnameResult {
	ob_base: crate::object::PyObjectHeader,
	values:  [String; 5],
}

static UNAME_SEQUENCE: std::sync::LazyLock<crate::object::PySequenceMethods> =
	std::sync::LazyLock::new(|| crate::object::PySequenceMethods {
		sq_length: Some(uname_result_len),
		sq_item: Some(uname_result_item),
		..crate::object::PySequenceMethods::EMPTY
	});

fn uname_result_type() -> *mut crate::object::PyType {
	static TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"posix.uname_result",
			std::mem::size_of::<PyUnameResult>(),
		);
		ty.tp_as_sequence = &*UNAME_SEQUENCE as *const crate::object::PySequenceMethods
			as *mut crate::object::PySequenceMethods;
		ty.tp_getattro = Some(uname_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn uname_result_len(_object: *mut PyObject) -> isize {
	UNAME_FIELDS.len() as isize
}

unsafe extern "C" fn uname_result_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	if index < 0 || index as usize >= UNAME_FIELDS.len() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"uname_result index out of range",
		);
	}
	let result = object.cast::<PyUnameResult>();
	let text = unsafe { &(*result).values[index as usize] };
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn uname_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if let Some(index) = UNAME_FIELDS.iter().position(|field| *field == name_text) {
		return unsafe { uname_result_item(object, index as isize) };
	}
	unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

fn uname_result_object(values: [String; 5]) -> *mut PyObject {
	Box::into_raw(Box::new(PyUnameResult {
		ob_base: crate::object::PyObjectHeader::new(uname_result_type()),
		values,
	}))
	.cast::<PyObject>()
}

const WAITID_FIELDS: [&str; 5] = ["si_pid", "si_uid", "si_signo", "si_status", "si_code"];

#[repr(C)]
struct PyWaitIdResult {
	ob_base: crate::object::PyObjectHeader,
	values:  [i64; 5],
}

static WAITID_SEQUENCE: std::sync::LazyLock<crate::object::PySequenceMethods> =
	std::sync::LazyLock::new(|| crate::object::PySequenceMethods {
		sq_length: Some(waitid_result_len),
		sq_item: Some(waitid_result_item),
		..crate::object::PySequenceMethods::EMPTY
	});

fn waitid_result_type() -> *mut crate::object::PyType {
	static TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = crate::object::PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"posix.waitid_result",
			std::mem::size_of::<PyWaitIdResult>(),
		);
		ty.tp_as_sequence = &*WAITID_SEQUENCE as *const crate::object::PySequenceMethods
			as *mut crate::object::PySequenceMethods;
		ty.tp_getattro = Some(waitid_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn waitid_result_len(_object: *mut PyObject) -> isize {
	WAITID_FIELDS.len() as isize
}

unsafe extern "C" fn waitid_result_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	if index < 0 || index as usize >= WAITID_FIELDS.len() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"waitid_result index out of range",
		);
	}
	let result = object.cast::<PyWaitIdResult>();
	unsafe { crate::abi::pon_const_int((*result).values[index as usize]) }
}

unsafe extern "C" fn waitid_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if let Some(index) = WAITID_FIELDS.iter().position(|field| *field == name_text) {
		return unsafe { waitid_result_item(object, index as isize) };
	}
	unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

fn waitid_result_object(info: &libc::siginfo_t) -> *mut PyObject {
	let values = [
		unsafe { info.si_pid() } as i64,
		unsafe { info.si_uid() } as i64,
		i64::from(info.si_signo),
		unsafe { info.si_status() } as i64,
		i64::from(info.si_code),
	];
	Box::into_raw(Box::new(PyWaitIdResult {
		ob_base: crate::object::PyObjectHeader::new(waitid_result_type()),
		values,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn os_statvfs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.statvfs expected one argument");
	}
	let path = match path_arg(args[0], "statvfs") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mut raw = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
	if unsafe { statvfs(c_path.as_ptr(), raw.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	statvfs_result_object(&unsafe { raw.assume_init() })
}

unsafe extern "C" fn os_fstatvfs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "fstatvfs") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let mut raw = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
	if unsafe { fstatvfs(fd, raw.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	statvfs_result_object(&unsafe { raw.assume_init() })
}

fn iov_max() -> usize {
	let value = unsafe { libc::sysconf(libc::_SC_IOV_MAX) };
	if value > 0 { value as usize } else { 16 }
}

fn iov_count_arg(len: usize) -> Result<libc::c_int, *mut PyObject> {
	if len > iov_max() || len > libc::c_int::MAX as usize {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			"too many buffers",
		));
	}
	Ok(len as libc::c_int)
}

fn readable_iovecs(object: *mut PyObject) -> Result<Vec<libc::iovec>, *mut PyObject> {
	let buffers =
		crate::abi::seq::sequence_to_vec(object).map_err(crate::abi::return_null_with_error)?;
	let _ = iov_count_arg(buffers.len())?;
	let mut iovecs = Vec::with_capacity(buffers.len());
	for buffer in buffers {
		let payload = readable_bytes_payload(crate::tag::untag_arg(buffer))?;
		iovecs.push(libc::iovec {
			iov_base: payload.as_ptr() as *mut libc::c_void,
			iov_len:  payload.len(),
		});
	}
	Ok(iovecs)
}

fn writable_iovecs(object: *mut PyObject) -> Result<Vec<libc::iovec>, *mut PyObject> {
	let buffers =
		crate::abi::seq::sequence_to_vec(object).map_err(crate::abi::return_null_with_error)?;
	let _ = iov_count_arg(buffers.len())?;
	let mut iovecs = Vec::with_capacity(buffers.len());
	for buffer in buffers {
		let (ptr, len) = writable_bytes_target(crate::tag::untag_arg(buffer))?;
		iovecs.push(libc::iovec { iov_base: ptr.cast::<libc::c_void>(), iov_len: len });
	}
	Ok(iovecs)
}

unsafe extern "C" fn os_readv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.readv expected two arguments");
	}
	let fd = match int_arg(args[0], "readv fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let mut iovecs = match writable_iovecs(args[1]) {
		Ok(iovecs) => iovecs,
		Err(error) => return error,
	};
	let count = unsafe { libc::readv(fd, iovecs.as_mut_ptr(), iovecs.len() as libc::c_int) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(count as i64) }
}

unsafe extern "C" fn os_writev(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.writev expected two arguments");
	}
	let fd = match int_arg(args[0], "writev fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let iovecs = match readable_iovecs(args[1]) {
		Ok(iovecs) => iovecs,
		Err(error) => return error,
	};
	let count = unsafe { libc::writev(fd, iovecs.as_ptr(), iovecs.len() as libc::c_int) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(count as i64) }
}

unsafe extern "C" fn os_preadv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if !(3..=4).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.preadv expected three or four arguments");
	}
	if let Some(flags) = args.get(3) {
		match int_arg(*flags, "preadv flags") {
			Ok(0) => {},
			Ok(_) => {
				return crate::abi::exc::raise_kind_error_text(
					ExceptionKind::NotImplementedError,
					"os.preadv flags are not supported by this platform",
				);
			},
			Err(error) => return error,
		}
	}
	let fd = match int_arg(args[0], "preadv fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let mut iovecs = match writable_iovecs(args[1]) {
		Ok(iovecs) => iovecs,
		Err(error) => return error,
	};
	let offset = match int_arg(args[2], "preadv offset") {
		Ok(offset) => offset as libc::off_t,
		Err(error) => return error,
	};
	let count =
		unsafe { libc::preadv(fd, iovecs.as_mut_ptr(), iovecs.len() as libc::c_int, offset) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(count as i64) }
}

unsafe extern "C" fn os_pwritev(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if !(3..=4).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.pwritev expected three or four arguments");
	}
	if let Some(flags) = args.get(3) {
		match int_arg(*flags, "pwritev flags") {
			Ok(0) => {},
			Ok(_) => {
				return crate::abi::exc::raise_kind_error_text(
					ExceptionKind::NotImplementedError,
					"os.pwritev flags are not supported by this platform",
				);
			},
			Err(error) => return error,
		}
	}
	let fd = match int_arg(args[0], "pwritev fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let iovecs = match readable_iovecs(args[1]) {
		Ok(iovecs) => iovecs,
		Err(error) => return error,
	};
	let offset = match int_arg(args[2], "pwritev offset") {
		Ok(offset) => offset as libc::off_t,
		Err(error) => return error,
	};
	let count = unsafe { libc::pwritev(fd, iovecs.as_ptr(), iovecs.len() as libc::c_int, offset) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(count as i64) }
}

fn ensure_empty_sequence(object: *mut PyObject, what: &str) -> Result<(), *mut PyObject> {
	let values =
		crate::abi::seq::sequence_to_vec(object).map_err(crate::abi::return_null_with_error)?;
	if values.is_empty() {
		Ok(())
	} else {
		Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::NotImplementedError,
			&format!("os.sendfile {what} are not supported"),
		))
	}
}

unsafe extern "C" fn os_sendfile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if !(4..=7).contains(&args.len()) {
		return crate::abi::return_null_with_error(
			"os.sendfile expected between four and seven arguments",
		);
	}
	let out_fd = match int_arg(args[0], "sendfile out_fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let in_fd = match int_arg(args[1], "sendfile in_fd") {
		Ok(fd) => fd as libc::c_int,
		Err(error) => return error,
	};
	let offset = match int_arg(args[2], "sendfile offset") {
		Ok(offset) => offset as libc::off_t,
		Err(error) => return error,
	};
	let count = match int_arg(args[3], "sendfile count") {
		Ok(count) if count >= 0 => count as libc::off_t,
		Ok(_) => {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"count must be non-negative",
			);
		},
		Err(error) => return error,
	};
	if let Some(headers) = args.get(4) {
		if let Err(error) = ensure_empty_sequence(*headers, "headers") {
			return error;
		}
	}
	if let Some(trailers) = args.get(5) {
		if let Err(error) = ensure_empty_sequence(*trailers, "trailers") {
			return error;
		}
	}
	let flags = match args.get(6) {
		Some(flags) => match int_arg(*flags, "sendfile flags") {
			Ok(flags) => flags as libc::c_int,
			Err(error) => return error,
		},
		None => 0,
	};
	#[cfg(target_os = "macos")]
	{
		let mut count = count;
		let result =
			unsafe { libc::sendfile(in_fd, out_fd, offset, &mut count, std::ptr::null_mut(), flags) };
		if result < 0 && count == 0 {
			return raise_errno(last_errno(), None);
		}
		unsafe { crate::abi::pon_const_int(count as i64) }
	}
	#[cfg(not(target_os = "macos"))]
	{
		// Linux sendfile(2) reports progress through the offset pointer and
		// returns the byte count; the Darwin-only flags word does not apply.
		let _ = flags;
		let mut offset = offset;
		let sent = unsafe { libc::sendfile(out_fd, in_fd, &mut offset, count as usize) };
		if sent < 0 {
			return raise_errno(last_errno(), None);
		}
		unsafe { crate::abi::pon_const_int(sent as i64) }
	}
}

unsafe extern "C" fn os_wait3(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.wait3 expected one argument");
	}
	let options = match int_arg(args[0], "wait3 options") {
		Ok(options) => options as libc::c_int,
		Err(error) => return error,
	};
	let mut status: libc::c_int = 0;
	let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
	let pid = unsafe { wait3(&mut status, options, usage.as_mut_ptr()) };
	if pid < 0 {
		return raise_errno(last_errno(), None);
	}
	wait_with_rusage_tuple(pid, status, unsafe { usage.assume_init() })
}

unsafe extern "C" fn os_wait4(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.wait4 expected two arguments");
	}
	let pid = match int_arg(args[0], "wait4 pid") {
		Ok(pid) => pid as libc::pid_t,
		Err(error) => return error,
	};
	let options = match int_arg(args[1], "wait4 options") {
		Ok(options) => options as libc::c_int,
		Err(error) => return error,
	};
	let mut status: libc::c_int = 0;
	let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
	let reaped = unsafe { wait4(pid, &mut status, options, usage.as_mut_ptr()) };
	if reaped < 0 {
		return raise_errno(last_errno(), None);
	}
	wait_with_rusage_tuple(reaped, status, unsafe { usage.assume_init() })
}

fn wait_with_rusage_tuple(
	pid: libc::pid_t,
	status: libc::c_int,
	usage: libc::rusage,
) -> *mut PyObject {
	let rusage = super::resource::rusage_object(super::resource::rusage_record(&usage));
	let mut items = [
		unsafe { crate::abi::pon_const_int(i64::from(pid)) },
		unsafe { crate::abi::pon_const_int(i64::from(status)) },
		rusage,
	];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn os_waitid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.waitid expected three arguments");
	}
	let idtype = match int_arg(args[0], "waitid idtype") {
		Ok(idtype) => idtype as libc::idtype_t,
		Err(error) => return error,
	};
	let id = match int_arg(args[1], "waitid id") {
		Ok(id) => id as libc::id_t,
		Err(error) => return error,
	};
	let options = match int_arg(args[2], "waitid options") {
		Ok(options) => options as libc::c_int,
		Err(error) => return error,
	};
	let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
	if unsafe { libc::waitid(idtype, id, info.as_mut_ptr(), options) } < 0 {
		return raise_errno(last_errno(), None);
	}
	let info = unsafe { info.assume_init() };
	if unsafe { info.si_pid() } == 0 {
		return unsafe { crate::abi::pon_none() };
	}
	waitid_result_object(&info)
}

unsafe extern "C" fn os_execv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.execv expected two arguments");
	}
	let path = match path_arg(args[0], "execv") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let (argv_store, argv_ptrs) = match argv_cstrings(args[1]) {
		Ok(argv) => argv,
		Err(error) => return error,
	};
	let _keepalive = argv_store;
	unsafe { libc::execv(c_path.as_ptr(), argv_ptrs.as_ptr().cast::<*const libc::c_char>()) };
	raise_errno(last_errno(), Some(&path))
}

unsafe extern "C" fn os_execve(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.execve expected three arguments");
	}
	let path = match path_arg(args[0], "execve") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let (argv_store, argv_ptrs) = match argv_cstrings(args[1]) {
		Ok(argv) => argv,
		Err(error) => return error,
	};
	let (env_store, env_ptrs) = match env_cstrings(args[2]) {
		Ok(env) => env,
		Err(error) => return error,
	};
	let (_argv_keepalive, _env_keepalive) = (argv_store, env_store);
	unsafe {
		libc::execve(
			c_path.as_ptr(),
			argv_ptrs.as_ptr().cast::<*const libc::c_char>(),
			env_ptrs.as_ptr().cast::<*const libc::c_char>(),
		)
	};
	raise_errno(last_errno(), Some(&path))
}

unsafe extern "C" fn os_spawnv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnv_common(argv, argc, SpawnFlavor::Path, false) }
}

unsafe extern "C" fn os_spawnve(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnv_common(argv, argc, SpawnFlavor::Path, true) }
}

unsafe extern "C" fn os_spawnvp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnv_common(argv, argc, SpawnFlavor::SearchPath, false) }
}

unsafe extern "C" fn os_spawnvpe(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnv_common(argv, argc, SpawnFlavor::SearchPath, true) }
}

unsafe extern "C" fn os_spawnl(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnl_common(argv, argc, SpawnFlavor::Path, false) }
}

unsafe extern "C" fn os_spawnle(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnl_common(argv, argc, SpawnFlavor::Path, true) }
}

unsafe extern "C" fn os_spawnlp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnl_common(argv, argc, SpawnFlavor::SearchPath, false) }
}

unsafe extern "C" fn os_spawnlpe(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { spawnl_common(argv, argc, SpawnFlavor::SearchPath, true) }
}

#[derive(Clone, Copy)]
enum SpawnFlavor {
	Path,
	SearchPath,
}

unsafe fn spawnv_common(
	argv: *mut *mut PyObject,
	argc: usize,
	flavor: SpawnFlavor,
	has_env: bool,
) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != if has_env { 4 } else { 3 } {
		return crate::abi::return_null_with_error("spawnv-family expected mode, path, argv[, env]");
	}
	let mode = match int_arg(args[0], "spawn mode") {
		Ok(mode) => mode,
		Err(error) => return error,
	};
	let path = match path_arg(args[1], "spawn path") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let (argv_store, argv_ptrs) = match argv_cstrings(args[2]) {
		Ok(argv) => argv,
		Err(error) => return error,
	};
	let env_data = if has_env {
		match env_cstrings(args[3]) {
			Ok(env) => Some(env),
			Err(error) => return error,
		}
	} else {
		None
	};
	spawn_mode_dispatch(mode, &path, flavor, argv_store, argv_ptrs, env_data)
}

unsafe fn spawnl_common(
	argv: *mut *mut PyObject,
	argc: usize,
	flavor: SpawnFlavor,
	has_env: bool,
) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	let minimum = if has_env { 4 } else { 3 };
	if args.len() < minimum {
		return crate::abi::return_null_with_error("spawnl-family expected mode, path, arg0, ...");
	}
	let mode = match int_arg(args[0], "spawn mode") {
		Ok(mode) => mode,
		Err(error) => return error,
	};
	let path = match path_arg(args[1], "spawn path") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let argv_object = build_tuple_from_slice(if has_env {
		&args[2..args.len() - 1]
	} else {
		&args[2..]
	});
	if argv_object.is_null() {
		return std::ptr::null_mut();
	}
	let (argv_store, argv_ptrs) = match argv_cstrings(argv_object) {
		Ok(argv) => argv,
		Err(error) => return error,
	};
	let env_data = if has_env {
		match env_cstrings(*args.last().expect("minimum length checked")) {
			Ok(env) => Some(env),
			Err(error) => return error,
		}
	} else {
		None
	};
	spawn_mode_dispatch(mode, &path, flavor, argv_store, argv_ptrs, env_data)
}

fn build_tuple_from_slice(values: &[*mut PyObject]) -> *mut PyObject {
	let mut owned = values.to_vec();
	unsafe { crate::abi::seq::pon_build_tuple(owned.as_mut_ptr(), owned.len()) }
}

fn current_env_cstrings() -> Result<(Vec<std::ffi::CString>, Vec<*mut libc::c_char>), *mut PyObject>
{
	use std::os::unix::ffi::OsStrExt;
	let mut strings = Vec::new();
	for (key, value) in std::env::vars_os() {
		let mut bytes = key.as_os_str().as_bytes().to_vec();
		bytes.push(b'=');
		bytes.extend_from_slice(value.as_os_str().as_bytes());
		let text = std::str::from_utf8(&bytes).map_err(|_| {
			crate::abi::exc::raise_kind_error_text(
				ExceptionKind::UnicodeDecodeError,
				"environment contains non-UTF-8 entry",
			)
		})?;
		strings.push(c_path(text)?);
	}
	let mut ptrs = strings
		.iter()
		.map(|value| value.as_ptr() as *mut libc::c_char)
		.collect::<Vec<_>>();
	ptrs.push(std::ptr::null_mut());
	Ok((strings, ptrs))
}

fn spawn_mode_dispatch(
	mode: i64,
	path: &str,
	flavor: SpawnFlavor,
	argv_store: Vec<std::ffi::CString>,
	argv_ptrs: Vec<*mut libc::c_char>,
	env_data: Option<(Vec<std::ffi::CString>, Vec<*mut libc::c_char>)>,
) -> *mut PyObject {
	let child = match mode {
		0 | 1 => spawn_process(path, flavor, argv_store, argv_ptrs, env_data),
		_ => {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"mode must be P_WAIT, P_NOWAIT, or P_NOWAITO",
			);
		},
	};
	let pid = match child {
		Ok(pid) => pid,
		Err(error) => return error,
	};
	if mode == 0 {
		let mut status = 0 as libc::c_int;
		if unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
			return raise_errno(last_errno(), None);
		}
		unsafe { crate::abi::pon_const_int(i64::from(libc::WEXITSTATUS(status))) }
	} else {
		unsafe { crate::abi::pon_const_int(i64::from(pid)) }
	}
}

fn spawn_process(
	path: &str,
	flavor: SpawnFlavor,
	argv_store: Vec<std::ffi::CString>,
	argv_ptrs: Vec<*mut libc::c_char>,
	env_data: Option<(Vec<std::ffi::CString>, Vec<*mut libc::c_char>)>,
) -> Result<libc::pid_t, *mut PyObject> {
	let c_path = c_path(path)?;
	let (env_store, env_ptrs) = match env_data {
		Some((store, ptrs)) => (store, ptrs),
		None => current_env_cstrings()?,
	};
	let (_argv_keepalive, _env_keepalive) = (argv_store, env_store);
	let mut pid: libc::pid_t = 0;
	let rc = match flavor {
		SpawnFlavor::Path => unsafe {
			libc::posix_spawn(
				&mut pid,
				c_path.as_ptr(),
				std::ptr::null(),
				std::ptr::null(),
				argv_ptrs.as_ptr(),
				env_ptrs.as_ptr(),
			)
		},
		SpawnFlavor::SearchPath => unsafe {
			libc::posix_spawnp(
				&mut pid,
				c_path.as_ptr(),
				std::ptr::null(),
				std::ptr::null(),
				argv_ptrs.as_ptr(),
				env_ptrs.as_ptr(),
			)
		},
	};
	if rc != 0 {
		Err(raise_errno(rc, Some(path)))
	} else {
		Ok(pid)
	}
}

unsafe extern "C" fn os_posix_spawn(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { posix_spawn_common(argv, argc, false) }
}

unsafe extern "C" fn os_posix_spawnp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { posix_spawn_common(argv, argc, true) }
}

/// Spawn options carried by `os.posix_spawn`'s keyword-only parameters,
/// decoded from the ten-slot layout the native keyword binder produces.
struct SpawnOptions {
	actions: super::posixsubprocess::FileActions,
	attr:    super::posixsubprocess::SpawnAttr,
}

impl SpawnOptions {
	/// Decodes slots 3..10 (`file_actions`, `setpgroup`, `resetids`,
	/// `setsid`, `setsigmask`, `setsigdef`, `scheduler`).  Empty/None slots
	/// leave the initialized-but-default actions/attr, which `posix_spawn`
	/// treats exactly like NULL pointers.
	fn parse(options: &[*mut PyObject]) -> Result<Self, *mut PyObject> {
		use super::posixsubprocess::{FileActions, POSIX_SPAWN_SETSID_FLAG, SpawnAttr};

		let mut actions = FileActions::new()
			.map_err(|errno| spawn_setup_error(errno, "posix_spawn_file_actions_init"))?;
		let mut attr =
			SpawnAttr::new().map_err(|errno| spawn_setup_error(errno, "posix_spawnattr_init"))?;
		let mut flags: libc::c_int = 0;

		if !is_none_value(options[0]) {
			for (index, entry) in spawn_sequence(options[0], "file_actions")?
				.iter()
				.enumerate()
			{
				let fields = spawn_sequence(*entry, "file_actions entry")?;
				let what = format!("file_actions[{index}]");
				let tag = match fields.first() {
					Some(&tag) => int_arg(tag, &what)?,
					None => return Err(raise_type_error(&format!("{what} must not be empty"))),
				};
				// Tag values are pon's os.POSIX_SPAWN_* constants (CPython's
				// enum order: OPEN=0, CLOSE=1, DUP2=2).
				let rc = match (tag, fields.len()) {
					(0, 5) => {
						let fd = spawn_fd(fields[1], &what)?;
						let open_path = path_arg(fields[2], "posix_spawn")?;
						let open_path = c_path(&open_path)?;
						let oflags = int_arg(fields[3], &what)? as libc::c_int;
						let mode = int_arg(fields[4], &what)? as libc::mode_t;
						// SAFETY: `actions` is initialized and `open_path` is
						// a live C string; libc validates the descriptor.
						unsafe {
							libc::posix_spawn_file_actions_addopen(
								actions.as_mut_ptr(),
								fd,
								open_path.as_ptr(),
								oflags,
								mode,
							)
						}
					},
					(1, 2) => {
						let fd = spawn_fd(fields[1], &what)?;
						// SAFETY: `actions` is initialized; libc validates `fd`.
						unsafe { libc::posix_spawn_file_actions_addclose(actions.as_mut_ptr(), fd) }
					},
					(2, 3) => {
						let fd = spawn_fd(fields[1], &what)?;
						let new_fd = spawn_fd(fields[2], &what)?;
						// SAFETY: `actions` is initialized; libc validates both descriptors.
						unsafe {
							libc::posix_spawn_file_actions_adddup2(actions.as_mut_ptr(), fd, new_fd)
						}
					},
					_ => {
						return Err(raise_type_error(&format!(
							"{what} is not a valid POSIX_SPAWN_OPEN/CLOSE/DUP2 tuple"
						)));
					},
				};
				if rc != 0 {
					return Err(spawn_setup_error(rc, "posix_spawn_file_actions"));
				}
			}
		}

		if !is_none_value(options[1]) {
			let pgroup = int_arg(options[1], "setpgroup")? as libc::pid_t;
			// SAFETY: `attr` is initialized.
			let rc = unsafe { libc::posix_spawnattr_setpgroup(attr.as_mut_ptr(), pgroup) };
			if rc != 0 {
				return Err(spawn_setup_error(rc, "posix_spawnattr_setpgroup"));
			}
			flags |= libc::POSIX_SPAWN_SETPGROUP as libc::c_int;
		}
		if !is_none_value(options[2]) && truth_arg(options[2])? {
			flags |= libc::POSIX_SPAWN_RESETIDS as libc::c_int;
		}
		if !is_none_value(options[3]) && truth_arg(options[3])? {
			flags |= POSIX_SPAWN_SETSID_FLAG;
		}
		if let Some(sigset) = spawn_sigset(options[4], "setsigmask")? {
			// SAFETY: `attr` is initialized and `sigset` is a built signal set.
			let rc = unsafe { libc::posix_spawnattr_setsigmask(attr.as_mut_ptr(), &sigset) };
			if rc != 0 {
				return Err(spawn_setup_error(rc, "posix_spawnattr_setsigmask"));
			}
			flags |= libc::POSIX_SPAWN_SETSIGMASK as libc::c_int;
		}
		if let Some(sigset) = spawn_sigset(options[5], "setsigdef")? {
			// SAFETY: `attr` is initialized and `sigset` is a built signal set.
			let rc = unsafe { libc::posix_spawnattr_setsigdefault(attr.as_mut_ptr(), &sigset) };
			if rc != 0 {
				return Err(spawn_setup_error(rc, "posix_spawnattr_setsigdefault"));
			}
			flags |= libc::POSIX_SPAWN_SETSIGDEF as libc::c_int;
		}
		if !is_none_value(options[6]) {
			return Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::NotImplementedError,
				"os.posix_spawn does not implement the scheduler parameter on this platform",
			));
		}

		if flags != 0 {
			// SAFETY: `attr` is initialized; flags are host spawn constants.
			let rc =
				unsafe { libc::posix_spawnattr_setflags(attr.as_mut_ptr(), flags as libc::c_short) };
			if rc != 0 {
				return Err(spawn_setup_error(rc, "posix_spawnattr_setflags"));
			}
		}
		Ok(Self { actions, attr })
	}
}

fn spawn_setup_error(errno: libc::c_int, context: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::OSError,
		&format!("{context} failed: [Errno {errno}]"),
	)
}

/// A `file_actions` entry or container as a slice of items (list or tuple).
fn spawn_sequence(object: *mut PyObject, what: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	crate::abi::seq::sequence_to_vec(object)
		.map_err(|_| raise_type_error(&format!("{what} must be a sequence")))
}

fn spawn_fd(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, what)?;
	if value < 0 || value > i64::from(libc::c_int::MAX) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			&format!("{what}: bad file descriptor"),
		));
	}
	Ok(value as libc::c_int)
}

/// Builds a signal set from a `setsigmask`/`setsigdef` iterable; `None` and
/// the empty sequence request no attribute (CPython's `()` default).
fn spawn_sigset(
	object: *mut PyObject,
	what: &str,
) -> Result<Option<libc::sigset_t>, *mut PyObject> {
	if is_none_value(object) {
		return Ok(None);
	}
	let signals = spawn_sequence(object, what)?;
	if signals.is_empty() {
		return Ok(None);
	}
	let mut sigset = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
	// SAFETY: `sigset` is an out-slot for the libc initializer.
	if unsafe { libc::sigemptyset(sigset.as_mut_ptr()) } != 0 {
		return Err(spawn_setup_error(last_errno(), "sigemptyset"));
	}
	// SAFETY: libc reported successful initialization.
	let mut sigset = unsafe { sigset.assume_init() };
	for (index, signal) in signals.iter().enumerate() {
		let signal = int_arg(*signal, &format!("{what}[{index}]"))? as libc::c_int;
		// SAFETY: `sigset` is initialized; libc validates the signal number.
		if unsafe { libc::sigaddset(&mut sigset, signal) } != 0 {
			return Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				&format!("{what}: invalid signal number {signal}"),
			));
		}
	}
	Ok(Some(sigset))
}

unsafe fn posix_spawn_common(
	argv: *mut *mut PyObject,
	argc: usize,
	search_path: bool,
) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	// Three slots: plain positional call.  Ten: the native keyword binder
	// flattened `file_actions`/`setpgroup`/`resetids`/`setsid`/`setsigmask`/
	// `setsigdef`/`scheduler` into their named slots (absent → None).
	if args.len() != 3 && args.len() != 10 {
		return raise_type_error(
			"os.posix_spawn expected path, argv, env, and keyword-only spawn options",
		);
	}
	let path = match path_arg(args[0], "posix_spawn") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let (argv_store, argv_ptrs) = match argv_cstrings(args[1]) {
		Ok(argv) => argv,
		Err(error) => return error,
	};
	// `env=None` inherits the live process environment (CPython 3.13+,
	// gh-113119); subprocess's `_posix_spawn` fast path passes it through.
	let env_data = if is_none_value(args[2]) {
		None
	} else {
		match env_cstrings(args[2]) {
			Ok(env) => Some(env),
			Err(error) => return error,
		}
	};
	let envp = env_data
		.as_ref()
		.map_or_else(super::posixsubprocess::inherited_envp, |(_, ptrs)| ptrs.as_ptr());
	let _argv_keepalive = argv_store;
	let options = if args.len() == 10 {
		match SpawnOptions::parse(&args[3..]) {
			Ok(options) => Some(options),
			Err(error) => return error,
		}
	} else {
		None
	};
	let (actions_ptr, attr_ptr) = options
		.as_ref()
		.map_or((std::ptr::null(), std::ptr::null()), |options| {
			(options.actions.as_ptr(), options.attr.as_ptr())
		});
	let mut pid: libc::pid_t = 0;
	let result = if search_path {
		unsafe {
			libc::posix_spawnp(
				&mut pid,
				c_path.as_ptr(),
				actions_ptr,
				attr_ptr,
				argv_ptrs.as_ptr(),
				envp,
			)
		}
	} else {
		unsafe {
			libc::posix_spawn(
				&mut pid,
				c_path.as_ptr(),
				actions_ptr,
				attr_ptr,
				argv_ptrs.as_ptr(),
				envp,
			)
		}
	};
	if result != 0 {
		return raise_errno(result, Some(&path));
	}
	unsafe { crate::abi::pon_const_int(i64::from(pid)) }
}

fn text_or_bytes_string(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("{what} must be str or bytes"),
		));
	}
	if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
		return Ok(text.to_owned());
	}
	if let Some(bytes) = bytes_payload(raw) {
		return std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| {
			crate::abi::exc::raise_kind_error_text(ExceptionKind::UnicodeDecodeError, what)
		});
	}
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		&format!("{what} must be str or bytes"),
	))
}

fn argv_cstrings(
	object: *mut PyObject,
) -> Result<(Vec<std::ffi::CString>, Vec<*mut libc::c_char>), *mut PyObject> {
	let items =
		crate::abi::seq::sequence_to_vec(object).map_err(crate::abi::return_null_with_error)?;
	if items.is_empty() {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			"argv must not be empty",
		));
	}
	let mut strings = Vec::with_capacity(items.len());
	for item in items {
		let text = text_or_bytes_string(item, "argv item")?;
		strings.push(c_path(&text)?);
	}
	let mut ptrs = strings
		.iter()
		.map(|value| value.as_ptr() as *mut libc::c_char)
		.collect::<Vec<_>>();
	ptrs.push(std::ptr::null_mut());
	Ok((strings, ptrs))
}

fn env_cstrings(
	object: *mut PyObject,
) -> Result<(Vec<std::ffi::CString>, Vec<*mut libc::c_char>), *mut PyObject> {
	let items_method =
		unsafe { crate::abi::pon_get_attr(object, intern("items"), std::ptr::null_mut()) };
	if items_method.is_null() {
		return Err(std::ptr::null_mut());
	}
	let pairs_object = unsafe { crate::abi::pon_call(items_method, std::ptr::null_mut(), 0) };
	if pairs_object.is_null() {
		return Err(std::ptr::null_mut());
	}
	let pairs =
		crate::abi::seq::sequence_to_vec(pairs_object).map_err(crate::abi::return_null_with_error)?;
	let mut strings = Vec::with_capacity(pairs.len());
	for pair in pairs {
		let kv =
			crate::abi::seq::sequence_to_vec(pair).map_err(crate::abi::return_null_with_error)?;
		if kv.len() != 2 {
			return Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"env items must be 2-item sequences",
			));
		}
		let key = text_or_bytes_string(kv[0], "env key")?;
		let value = text_or_bytes_string(kv[1], "env value")?;
		strings.push(c_path(&format!("{key}={value}"))?);
	}
	let mut ptrs = strings
		.iter()
		.map(|value| value.as_ptr() as *mut libc::c_char)
		.collect::<Vec<_>>();
	ptrs.push(std::ptr::null_mut());
	Ok((strings, ptrs))
}

/// `os.utime(path, times=None, *, ns=(), dir_fd=None, follow_symlinks=True)`
/// over `utimensat(2)`.  The native keyword binder flattens the keyword-only
/// options into slots 2..5 (absent → None); `shutil.copystat` passes `ns=`
/// and `follow_symlinks=`.
unsafe extern "C" fn os_utime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if !(1..=5).contains(&args.len()) {
		return crate::abi::return_null_with_error(
			"os.utime expected path and optional times/ns options",
		);
	}
	let path = match path_arg(args[0], "utime") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let times_arg = args.get(1).copied().filter(|value| !is_none_value(*value));
	let ns_arg = args.get(2).copied().filter(|value| !is_none_value(*value));
	if times_arg.is_some() && ns_arg.is_some() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			"utime: you may specify either 'times' or 'ns' but not both",
		);
	}
	if args
		.get(3)
		.copied()
		.is_some_and(|value| !is_none_value(value))
	{
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::NotImplementedError,
			"utime: dir_fd is not supported",
		);
	}
	let follow_symlinks = match args.get(4).copied().filter(|value| !is_none_value(*value)) {
		Some(value) => match truth_arg(value) {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => true,
	};
	let mut times = [libc::timespec { tv_sec: 0, tv_nsec: 0 }; 2];
	let times_ptr = if let Some(ns_value) = ns_arg {
		// `ns` carries exact integer nanoseconds.
		let values = match crate::abi::seq::sequence_to_vec(ns_value) {
			Ok(values) if values.len() == 2 => values,
			Ok(_) => {
				return crate::abi::exc::raise_kind_error_text(
					ExceptionKind::TypeError,
					"utime: 'ns' must be a tuple of two ints",
				);
			},
			Err(message) => return crate::abi::return_null_with_error(message),
		};
		for (slot, value) in times.iter_mut().zip(values) {
			let nanos = match int_arg(value, "utime") {
				Ok(nanos) => nanos,
				Err(error) => return error,
			};
			slot.tv_sec = nanos.div_euclid(1_000_000_000) as libc::time_t;
			slot.tv_nsec = nanos.rem_euclid(1_000_000_000) as libc::c_long;
		}
		times.as_ptr()
	} else if let Some(times_value) = times_arg {
		let values = match crate::abi::seq::sequence_to_vec(times_value) {
			Ok(values) if values.len() == 2 => values,
			Ok(_) => {
				return crate::abi::exc::raise_kind_error_text(
					ExceptionKind::TypeError,
					"utime: 'times' must be either a tuple of two ints or None",
				);
			},
			Err(message) => return crate::abi::return_null_with_error(message),
		};
		for (slot, value) in times.iter_mut().zip(values) {
			let seconds = match seconds_float(value, "utime") {
				Ok(seconds) => seconds,
				Err(error) => return error,
			};
			let whole = seconds.floor();
			let nanos = ((seconds - whole) * 1e9).round();
			slot.tv_sec = whole as libc::time_t;
			slot.tv_nsec = nanos as libc::c_long;
		}
		times.as_ptr()
	} else {
		std::ptr::null()
	};
	let flags = if follow_symlinks {
		0
	} else {
		libc::AT_SYMLINK_NOFOLLOW
	};
	if unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times_ptr, flags) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

fn seconds_float(object: *mut PyObject, what: &str) -> Result<f64, *mut PyObject> {
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() {
		return Err(std::ptr::null_mut());
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(raw) } {
		return Ok(value);
	}
	unsafe { crate::types::int::to_bigint_including_bool(raw) }
		.and_then(|value| num_traits::ToPrimitive::to_f64(&value))
		.ok_or_else(|| {
			crate::abi::exc::raise_kind_error_text(
				ExceptionKind::TypeError,
				&format!("{what}: numeric value required"),
			)
		})
}

/// `os.chdir(path)` over `chdir(2)`.
unsafe extern "C" fn os_chdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.chdir expected one argument");
	}
	let path = match path_arg(args[0], "chdir") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::chdir(c_path.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.getcwd()` over `std::env::current_dir` (`sysconfig` calls it at
/// module scope via `_safe_realpath(os.getcwd())`).  Non-UTF-8 components
/// are decoded lossily rather than with CPython's `surrogateescape`.
unsafe extern "C" fn os_getcwd(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	match std::env::current_dir() {
		Ok(path) => {
			let text = path.to_string_lossy();
			// SAFETY: String allocation helper follows the NULL-sentinel contract.
			unsafe { pon_const_str(text.as_ptr(), text.len()) }
		},
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), None),
	}
}

/// `os.getpid()` over `std::process::id` (`test.support.os_helper` reads it
/// at module body: `TESTFN_ASCII` embeds the pid to disambiguate parallel
/// test runs).
unsafe extern "C" fn os_getpid(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(i64::from(std::process::id())) }
}
unsafe extern "C" fn os_exit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os._exit expected one argument");
	}
	let status = match int_arg(args[0], "_exit status") {
		Ok(status) => status,
		Err(error) => return error,
	};
	unsafe { libc::_exit(status as libc::c_int) }
}

unsafe extern "C" fn os_abort(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.abort expected no arguments");
	}
	unsafe { libc::abort() }
}

unsafe extern "C" fn os_getcwdb(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.getcwdb expected no arguments");
	}
	use std::os::unix::ffi::OsStrExt;
	match std::env::current_dir() {
		Ok(path) => {
			let bytes = path.as_os_str().as_bytes();
			unsafe { crate::abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
		},
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), None),
	}
}

macro_rules! noarg_int_fn {
	($name:ident, $py:literal, $expr:expr) => {
		unsafe extern "C" fn $name(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			if argc != 0 {
				return crate::abi::return_null_with_error(concat!(
					"os.",
					$py,
					" expected no arguments"
				));
			}
			let value = $expr;
			unsafe { crate::abi::pon_const_int(i64::from(value)) }
		}
	};
}

noarg_int_fn!(os_getegid, "getegid", unsafe { libc::getegid() });
noarg_int_fn!(os_geteuid, "geteuid", unsafe { libc::geteuid() });
noarg_int_fn!(os_getgid, "getgid", unsafe { libc::getgid() });
noarg_int_fn!(os_getpgrp, "getpgrp", unsafe { libc::getpgrp() });
noarg_int_fn!(os_getppid, "getppid", unsafe { libc::getppid() });

unsafe extern "C" fn os_cpu_count(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.cpu_count expected no arguments");
	}
	match std::thread::available_parallelism() {
		Ok(count) => unsafe { crate::abi::pon_const_int(count.get() as i64) },
		Err(_) => unsafe { crate::abi::pon_none() },
	}
}

unsafe extern "C" fn os_getgroups(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.getgroups expected no arguments");
	}
	let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	let mut groups = vec![0 as libc::gid_t; count as usize];
	let count = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	let mut objects = Vec::with_capacity(count as usize);
	for group in groups.into_iter().take(count as usize) {
		let object = unsafe { crate::abi::pon_const_int(i64::from(group)) };
		if object.is_null() {
			return std::ptr::null_mut();
		}
		objects.push(object);
	}
	unsafe { crate::abi::seq::pon_build_list(objects.as_mut_ptr(), objects.len()) }
}

unsafe extern "C" fn os_getloadavg(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.getloadavg expected no arguments");
	}
	let mut loads = [0.0 as libc::c_double; 3];
	if unsafe { libc::getloadavg(loads.as_mut_ptr(), loads.len() as libc::c_int) } != 3 {
		return raise_errno(last_errno(), None);
	}
	let mut items = [
		unsafe { crate::abi::number::pon_const_float(loads[0]) },
		unsafe { crate::abi::number::pon_const_float(loads[1]) },
		unsafe { crate::abi::number::pon_const_float(loads[2]) },
	];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn os_getlogin(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.getlogin expected no arguments");
	}
	let ptr = unsafe { libc::getlogin() };
	if ptr.is_null() {
		return raise_errno(last_errno(), None);
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn os_ctermid(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.ctermid expected no arguments");
	}
	let ptr = unsafe { ctermid(std::ptr::null_mut()) };
	if ptr.is_null() {
		return raise_errno(last_errno(), None);
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn os_getpgid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.getpgid expected one argument");
	}
	let pid = match int_arg(args[0], "getpgid pid") {
		Ok(pid) => pid,
		Err(error) => return error,
	};
	let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
	if pgid < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(pgid)) }
}

unsafe extern "C" fn os_getsid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.getsid expected one argument");
	}
	let pid = match int_arg(args[0], "getsid pid") {
		Ok(pid) => pid,
		Err(error) => return error,
	};
	let sid = unsafe { libc::getsid(pid as libc::pid_t) };
	if sid < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(sid)) }
}

unsafe extern "C" fn os_getpriority(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.getpriority expected two arguments");
	}
	let which = match int_arg(args[0], "getpriority which") {
		Ok(which) => which,
		Err(error) => return error,
	};
	let who = match int_arg(args[1], "getpriority who") {
		Ok(who) => who,
		Err(error) => return error,
	};
	let value = unsafe { libc::getpriority(which as _, who as libc::id_t) };
	if value == -1 && last_errno() != 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

/// `os.kill(pid, sig)` over `kill(2)`, raising the PEP 3151 errno subclass.
unsafe extern "C" fn os_kill(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.kill expected two arguments");
	}
	let pid = match int_arg(args[0], "kill pid") {
		Ok(pid) => pid,
		Err(error) => return error,
	};
	let sig = match int_arg(args[1], "kill sig") {
		Ok(sig) => sig,
		Err(error) => return error,
	};
	// SAFETY: Plain syscall; the kernel validates pid and signal.
	if unsafe { libc::kill(pid as libc::pid_t, sig as libc::c_int) } < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.killpg(pgid, sig)` over `killpg(2)`, raising the PEP 3151 errno
/// subclass.
unsafe extern "C" fn os_killpg(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.killpg expected two arguments");
	}
	let pgid = match int_arg(args[0], "killpg pgid") {
		Ok(pgid) => pgid,
		Err(error) => return error,
	};
	let sig = match int_arg(args[1], "killpg sig") {
		Ok(sig) => sig,
		Err(error) => return error,
	};
	// SAFETY: Plain syscall; the kernel validates process group and signal.
	if unsafe { libc::killpg(pgid as libc::pid_t, sig as libc::c_int) } < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}
unsafe extern "C" fn os_fork(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.fork expected no arguments");
	}
	let callbacks = at_fork_snapshot();
	if let Err(error) = call_at_fork_callbacks(&callbacks.before, true) {
		return error;
	}
	let pid = unsafe { libc::fork() };
	if pid == 0 {
		if let Err(error) = call_at_fork_callbacks(&callbacks.after_in_child, false) {
			return error;
		}
	} else {
		let fork_errno = (pid < 0).then(last_errno);
		if let Err(error) = call_at_fork_callbacks(&callbacks.after_in_parent, false) {
			return error;
		}
		if let Some(errno) = fork_errno {
			return raise_errno(errno, None);
		}
	}
	unsafe { crate::abi::pon_const_int(i64::from(pid)) }
}

macro_rules! one_id_none_fn {
	($name:ident, $py:literal, $call:path, $ty:ty) => {
		unsafe extern "C" fn $name(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let args = unsafe { call_args(argv, argc) };
			if args.len() != 1 {
				return crate::abi::return_null_with_error(concat!(
					"os.",
					$py,
					" expected one argument"
				));
			}
			let value = match int_arg(args[0], concat!($py, " value")) {
				Ok(value) => value,
				Err(error) => return error,
			};
			if unsafe { $call(value as $ty) } < 0 {
				return raise_errno(last_errno(), None);
			}
			unsafe { crate::abi::pon_none() }
		}
	};
}

one_id_none_fn!(os_setegid, "setegid", libc::setegid, libc::gid_t);
one_id_none_fn!(os_seteuid, "seteuid", libc::seteuid, libc::uid_t);
one_id_none_fn!(os_setgid, "setgid", libc::setgid, libc::gid_t);
one_id_none_fn!(os_setuid, "setuid", libc::setuid, libc::uid_t);

unsafe extern "C" fn os_setregid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.setregid expected two arguments");
	}
	let rgid = match int_arg(args[0], "setregid rgid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let egid = match int_arg(args[1], "setregid egid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::setregid(rgid as libc::gid_t, egid as libc::gid_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_setreuid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.setreuid expected two arguments");
	}
	let ruid = match int_arg(args[0], "setreuid ruid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let euid = match int_arg(args[1], "setreuid euid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::setreuid(ruid as libc::uid_t, euid as libc::uid_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_setpgid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.setpgid expected two arguments");
	}
	let pid = match int_arg(args[0], "setpgid pid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let pgrp = match int_arg(args[1], "setpgid pgrp") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::setpgid(pid as libc::pid_t, pgrp as libc::pid_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_setpgrp(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.setpgrp expected no arguments");
	}
	if unsafe { libc::setpgid(0, 0) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_setsid(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.setsid expected no arguments");
	}
	let sid = unsafe { libc::setsid() };
	if sid < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(sid)) }
}

unsafe extern "C" fn os_nice(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.nice expected one argument");
	}
	let increment = match int_arg(args[0], "nice increment") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let value = unsafe { libc::nice(increment as libc::c_int) };
	if value == -1 && last_errno() != 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

unsafe extern "C" fn os_setpriority(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.setpriority expected three arguments");
	}
	let which = match int_arg(args[0], "setpriority which") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let who = match int_arg(args[1], "setpriority who") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let priority = match int_arg(args[2], "setpriority priority") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::setpriority(which as _, who as libc::id_t, priority as libc::c_int) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

/// `os.readlink(path)` over `std::fs::read_link` (`posixpath.realpath`'s
/// symlink resolution, reached from `sysconfig._safe_realpath`).  Non-link
/// paths surface the host errno (EINVAL) like `readlink(2)`.
unsafe extern "C" fn os_readlink(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.readlink expected one argument");
	}
	let path = match path_arg(args[0], "readlink") {
		Ok(path) => path,
		Err(error) => return error,
	};
	match std::fs::read_link(&path) {
		Ok(target) => {
			let text = target.to_string_lossy();
			// SAFETY: String allocation helper follows the NULL-sentinel contract.
			unsafe { pon_const_str(text.as_ptr(), text.len()) }
		},
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
	}
}
unsafe extern "C" fn os_fchdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "fchdir") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::fchdir(fd) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_fchmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.fchmod expected two arguments");
	}
	let fd = match int_arg(args[0], "fchmod fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mode = match int_arg(args[1], "fchmod mode") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::fchmod(fd as libc::c_int, mode as libc::mode_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_fchown(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.fchown expected three arguments");
	}
	let fd = match int_arg(args[0], "fchown fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let uid = match int_arg(args[1], "fchown uid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let gid = match int_arg(args[2], "fchown gid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::fchown(fd as libc::c_int, uid as libc::uid_t, gid as libc::gid_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_fsync(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "fsync") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::fsync(fd) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_ftruncate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.ftruncate expected two arguments");
	}
	let fd = match int_arg(args[0], "ftruncate fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let length = match int_arg(args[1], "ftruncate length") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::ftruncate(fd as libc::c_int, length as libc::off_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_chown(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	chown_like(argv, argc, "chown", libc::chown)
}

unsafe extern "C" fn os_lchown(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	chown_like(argv, argc, "lchown", libc::lchown)
}

fn chown_like(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	call: unsafe extern "C" fn(*const libc::c_char, libc::uid_t, libc::gid_t) -> libc::c_int,
) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error(format!("os.{name} expected three arguments"));
	}
	let path = match path_arg(args[0], name) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let uid = match int_arg(args[1], &format!("{name} uid")) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let gid = match int_arg(args[2], &format!("{name} gid")) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { call(c_path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn os_chflags(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	chflags_like(argv, argc, "chflags", libc::chflags)
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn os_lchflags(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	chflags_like(argv, argc, "lchflags", lchflags)
}

#[cfg(target_os = "macos")]
fn chflags_like(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	call: unsafe extern "C" fn(*const libc::c_char, libc::c_uint) -> libc::c_int,
) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error(format!("os.{name} expected two arguments"));
	}
	let path = match path_arg(args[0], name) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let flags = match int_arg(args[1], &format!("{name} flags")) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { call(c_path.as_ptr(), flags as libc::c_uint) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn os_lchmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.lchmod expected two arguments");
	}
	let path = match path_arg(args[0], "lchmod") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match int_arg(args[1], "lchmod mode") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { lchmod(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_chroot(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.chroot expected one argument");
	}
	let path = match path_arg(args[0], "chroot") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::chroot(c_path.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_truncate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.truncate expected two arguments");
	}
	let path = match path_arg(args[0], "truncate") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let length = match int_arg(args[1], "truncate length") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::truncate(c_path.as_ptr(), length as libc::off_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_rename(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	rename_like(argv, argc, "rename")
}

unsafe extern "C" fn os_replace(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	rename_like(argv, argc, "replace")
}

fn rename_like(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() < 2 || args.len() > 4 {
		return crate::abi::return_null_with_error(format!("os.{name} expected two paths"));
	}
	if let Err(error) = reject_dir_fd(args, 2, name).and_then(|()| reject_dir_fd(args, 3, name)) {
		return error;
	}
	let src = match path_arg(args[0], name) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let dst = match path_arg(args[1], name) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_src = match c_path(&src) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_dst = match c_path(&dst) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::rename(c_src.as_ptr(), c_dst.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&src));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_link(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() < 2 || args.len() > 5 {
		return crate::abi::return_null_with_error("os.link expected two paths");
	}
	for index in 2..args.len() {
		if optional_arg(args, index).is_some() {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::NotImplementedError,
				"os.link optional fd/follow_symlinks arguments are unavailable",
			);
		}
	}
	let src = match path_arg(args[0], "link") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let dst = match path_arg(args[1], "link") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_src = match c_path(&src) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_dst = match c_path(&dst) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::link(c_src.as_ptr(), c_dst.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&src));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_symlink(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() < 2 || args.len() > 4 {
		return crate::abi::return_null_with_error("os.symlink expected source and destination");
	}
	if let Err(error) = reject_dir_fd(args, 3, "symlink") {
		return error;
	}
	let src = match path_arg(args[0], "symlink") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let dst = match path_arg(args[1], "symlink") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_src = match c_path(&src) {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_dst = match c_path(&dst) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::symlink(c_src.as_ptr(), c_dst.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&dst));
	}
	unsafe { crate::abi::pon_none() }
}

fn one_fd_arg(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<libc::c_int, *mut PyObject> {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return Err(crate::abi::return_null_with_error(format!("os.{name} expected one argument")));
	}
	int_arg(args[0], &format!("{name} fd")).map(|fd| fd as libc::c_int)
}

unsafe extern "C" fn os_closerange(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.closerange expected two arguments");
	}
	let low = match int_arg(args[0], "closerange low") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let high = match int_arg(args[1], "closerange high") {
		Ok(value) => value,
		Err(error) => return error,
	};
	for fd in low..high {
		if fd >= 0 && fd <= i64::from(libc::c_int::MAX) {
			unsafe { libc::close(fd as libc::c_int) };
		}
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_get_inheritable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "get_inheritable") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
	if flags < 0 {
		return raise_errno(last_errno(), None);
	}
	bool_object((flags & libc::FD_CLOEXEC) == 0)
}

unsafe extern "C" fn os_set_inheritable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.set_inheritable expected two arguments");
	}
	let fd = match int_arg(args[0], "set_inheritable fd") {
		Ok(value) => value as libc::c_int,
		Err(error) => return error,
	};
	let inheritable = match truth_arg(args[1]) {
		Ok(value) => value,
		Err(error) => return error,
	};
	if let Err(errno) = set_fd_cloexec(fd, !inheritable) {
		return raise_errno(errno, None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_get_blocking(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "get_blocking") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
	if flags < 0 {
		return raise_errno(last_errno(), None);
	}
	bool_object((flags & libc::O_NONBLOCK) == 0)
}

unsafe extern "C" fn os_set_blocking(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.set_blocking expected two arguments");
	}
	let fd = match int_arg(args[0], "set_blocking fd") {
		Ok(value) => value as libc::c_int,
		Err(error) => return error,
	};
	let blocking = match truth_arg(args[1]) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
	if flags < 0 {
		return raise_errno(last_errno(), None);
	}
	let new_flags = if blocking {
		flags & !libc::O_NONBLOCK
	} else {
		flags | libc::O_NONBLOCK
	};
	if unsafe { libc::fcntl(fd, libc::F_SETFL, new_flags) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_mkfifo(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.is_empty() || args.len() > 3 {
		return crate::abi::return_null_with_error("os.mkfifo expected path and optional mode");
	}
	let path = match path_arg(args[0], "mkfifo") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match optional_arg(args, 1).map(|object| int_arg(object, "mkfifo mode")) {
		None => 0o666,
		Some(Ok(value)) => value,
		Some(Err(error)) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 2, "mkfifo") {
		return error;
	}
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe { libc::mkfifo(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_pread(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.pread expected three arguments");
	}
	let fd = match int_arg(args[0], "pread fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let size = match int_arg(args[1], "pread size") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let offset = match int_arg(args[2], "pread offset") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if size < 0 {
		return raise_errno(libc::EINVAL, None);
	}
	let mut buffer = vec![0u8; size as usize];
	let count = unsafe {
		libc::pread(
			fd as libc::c_int,
			buffer.as_mut_ptr().cast(),
			buffer.len(),
			offset as libc::off_t,
		)
	};
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::str_::pon_const_bytes(buffer.as_ptr(), count as usize) }
}

unsafe extern "C" fn os_pwrite(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.pwrite expected three arguments");
	}
	let fd = match int_arg(args[0], "pwrite fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let data = crate::tag::untag_arg(args[1]);
	let payload = match readable_bytes_payload(data) {
		Ok(payload) => payload,
		Err(error) => return error,
	};
	let offset = match int_arg(args[2], "pwrite offset") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let count = unsafe {
		libc::pwrite(fd as libc::c_int, payload.as_ptr().cast(), payload.len(), offset as libc::off_t)
	};
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(count as i64) }
}

unsafe extern "C" fn os_major(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let dev = match single_int_arg(argv, argc, "major") {
		Ok(value) => value as u64,
		Err(error) => return error,
	};
	unsafe { crate::abi::pon_const_int(((dev >> 24) & 0xff) as i64) }
}

unsafe extern "C" fn os_minor(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let dev = match single_int_arg(argv, argc, "minor") {
		Ok(value) => value as u64,
		Err(error) => return error,
	};
	unsafe { crate::abi::pon_const_int((dev & 0x00ff_ffff) as i64) }
}

unsafe extern "C" fn os_makedev(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.makedev expected two arguments");
	}
	let major = match int_arg(args[0], "makedev major") {
		Ok(value) => value as u64,
		Err(error) => return error,
	};
	let minor = match int_arg(args[1], "makedev minor") {
		Ok(value) => value as u64,
		Err(error) => return error,
	};
	unsafe { crate::abi::pon_const_int((((major & 0xff) << 24) | (minor & 0x00ff_ffff)) as i64) }
}

fn single_int_arg(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<i64, *mut PyObject> {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return Err(crate::abi::return_null_with_error(format!("os.{name} expected one argument")));
	}
	int_arg(args[0], name)
}

unsafe extern "C" fn os_sched_get_priority_max(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let policy = match single_int_arg(argv, argc, "sched_get_priority_max") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let value = unsafe { libc::sched_get_priority_max(policy as libc::c_int) };
	if value < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

unsafe extern "C" fn os_sched_get_priority_min(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let policy = match single_int_arg(argv, argc, "sched_get_priority_min") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let value = unsafe { libc::sched_get_priority_min(policy as libc::c_int) };
	if value < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

unsafe extern "C" fn os_sched_yield(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.sched_yield expected no arguments");
	}
	if unsafe { libc::sched_yield() } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_sync(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.sync expected no arguments");
	}
	unsafe { libc::sync() };
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_system(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.system expected one argument");
	}
	let command = match path_arg(args[0], "system") {
		Ok(command) => command,
		Err(error) => return error,
	};
	let c_command = match std::ffi::CString::new(command) {
		Ok(command) => command,
		Err(_) => {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"embedded null byte",
			);
		},
	};
	let status = unsafe { libc::system(c_command.as_ptr()) };
	unsafe { crate::abi::pon_const_int(i64::from(status)) }
}

unsafe extern "C" fn os_tcgetpgrp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "tcgetpgrp") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let pgid = unsafe { libc::tcgetpgrp(fd) };
	if pgid < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(pgid)) }
}

unsafe extern "C" fn os_tcsetpgrp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.tcsetpgrp expected two arguments");
	}
	let fd = match int_arg(args[0], "tcsetpgrp fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let pgid = match int_arg(args[1], "tcsetpgrp pgid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::tcsetpgrp(fd as libc::c_int, pgid as libc::pid_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_ttyname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "ttyname") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let ptr = unsafe { libc::ttyname(fd) };
	if ptr.is_null() {
		return raise_errno(last_errno(), None);
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn os_uname(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.uname expected no arguments");
	}
	let mut uts = std::mem::MaybeUninit::<libc::utsname>::zeroed();
	if unsafe { libc::uname(uts.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	let uts = unsafe { uts.assume_init() };
	let fields = [
		c_array_string(&uts.sysname),
		c_array_string(&uts.nodename),
		c_array_string(&uts.release),
		c_array_string(&uts.version),
		c_array_string(&uts.machine),
	];
	uname_result_object(fields)
}

fn c_array_string(buffer: &[libc::c_char]) -> String {
	let ptr = buffer.as_ptr();
	unsafe { std::ffi::CStr::from_ptr(ptr) }
		.to_string_lossy()
		.into_owned()
}

unsafe extern "C" fn os_times(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.times expected no arguments");
	}
	let mut tms = std::mem::MaybeUninit::<libc::tms>::zeroed();
	let elapsed = unsafe { libc::times(tms.as_mut_ptr()) };
	if elapsed == !0 {
		return raise_errno(last_errno(), None);
	}
	let tms = unsafe { tms.assume_init() };
	let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
	let values = [
		tms.tms_utime as f64 / ticks,
		tms.tms_stime as f64 / ticks,
		tms.tms_cutime as f64 / ticks,
		tms.tms_cstime as f64 / ticks,
		elapsed as f64 / ticks,
	];
	times_result_object(values)
}

unsafe extern "C" fn os_wait(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.wait expected no arguments");
	}
	let mut status = 0 as libc::c_int;
	let pid = unsafe { libc::wait(&mut status) };
	if pid < 0 {
		return raise_errno(last_errno(), None);
	}
	let mut items = [unsafe { crate::abi::pon_const_int(i64::from(pid)) }, unsafe {
		crate::abi::pon_const_int(i64::from(status))
	}];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn os_posix_openpt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let flags = match single_int_arg(argv, argc, "posix_openpt") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let fd = unsafe { libc::posix_openpt(flags as libc::c_int) };
	if fd < 0 {
		return raise_errno(last_errno(), None);
	}
	if let Err(errno) = set_fd_cloexec(fd, true) {
		unsafe { libc::close(fd) };
		return raise_errno(errno, None);
	}
	unsafe { crate::abi::pon_const_int(i64::from(fd)) }
}

unsafe extern "C" fn os_grantpt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "grantpt") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::grantpt(fd) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_unlockpt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "unlockpt") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::unlockpt(fd) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_ptsname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "ptsname") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let ptr = unsafe { libc::ptsname(fd) };
	if ptr.is_null() {
		return raise_errno(last_errno(), None);
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn os_openpty(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.openpty expected no arguments");
	}
	let mut master = 0 as libc::c_int;
	let mut slave = 0 as libc::c_int;
	if unsafe {
		libc::openpty(
			&mut master,
			&mut slave,
			std::ptr::null_mut(),
			std::ptr::null_mut(),
			std::ptr::null_mut(),
		)
	} < 0
	{
		return raise_errno(last_errno(), None);
	}
	if let Err(errno) = set_fd_cloexec(master, true).and_then(|()| set_fd_cloexec(slave, true)) {
		unsafe {
			libc::close(master);
			libc::close(slave);
		}
		return raise_errno(errno, None);
	}
	let mut items = [unsafe { crate::abi::pon_const_int(i64::from(master)) }, unsafe {
		crate::abi::pon_const_int(i64::from(slave))
	}];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn os_forkpty(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("os.forkpty expected no arguments");
	}
	let callbacks = at_fork_snapshot();
	if let Err(error) = call_at_fork_callbacks(&callbacks.before, true) {
		return error;
	}
	let mut master = 0 as libc::c_int;
	let pid = unsafe {
		libc::forkpty(&mut master, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
	};
	if pid == 0 {
		if let Err(error) = call_at_fork_callbacks(&callbacks.after_in_child, false) {
			return error;
		}
	} else {
		let fork_errno = (pid < 0).then(last_errno);
		if let Err(error) = call_at_fork_callbacks(&callbacks.after_in_parent, false) {
			if pid > 0 {
				unsafe { libc::close(master) };
			}
			return error;
		}
		if let Some(errno) = fork_errno {
			return raise_errno(errno, None);
		}
	}
	if pid > 0 {
		if let Err(errno) = set_fd_cloexec(master, true) {
			unsafe { libc::close(master) };
			return raise_errno(errno, None);
		}
	}
	let mut items = [unsafe { crate::abi::pon_const_int(i64::from(pid)) }, unsafe {
		crate::abi::pon_const_int(i64::from(master))
	}];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn os_initgroups(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.initgroups expected two arguments");
	}
	let user = match path_arg(args[0], "initgroups user") {
		Ok(user) => user,
		Err(error) => return error,
	};
	let gid = match int_arg(args[1], "initgroups gid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_user = match std::ffi::CString::new(user) {
		Ok(user) => user,
		Err(_) => {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"embedded null byte",
			);
		},
	};
	if unsafe { libc::initgroups(c_user.as_ptr(), gid as _) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

/// `getgrouplist(3)` group-buffer element: `int` on Darwin, `gid_t` on Linux.
#[cfg(target_os = "macos")]
type GroupId = libc::c_int;
#[cfg(not(target_os = "macos"))]
type GroupId = libc::gid_t;

unsafe extern "C" fn os_getgrouplist(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.getgrouplist expected two arguments");
	}
	let user = match path_arg(args[0], "getgrouplist user") {
		Ok(user) => user,
		Err(error) => return error,
	};
	let gid = match int_arg(args[1], "getgrouplist gid") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_user = match std::ffi::CString::new(user) {
		Ok(user) => user,
		Err(_) => {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::ValueError,
				"embedded null byte",
			);
		},
	};
	let mut ngroups = 0 as libc::c_int;
	unsafe { libc::getgrouplist(c_user.as_ptr(), gid as _, std::ptr::null_mut(), &mut ngroups) };
	if ngroups <= 0 {
		ngroups = 16;
	}
	loop {
		let mut groups = vec![0 as GroupId; ngroups as usize];
		let mut capacity = ngroups;
		let rc = unsafe {
			libc::getgrouplist(c_user.as_ptr(), gid as _, groups.as_mut_ptr(), &mut capacity)
		};
		if rc >= 0 {
			let mut objects = Vec::with_capacity(capacity as usize);
			for group in groups.into_iter().take(capacity as usize) {
				let object = unsafe { crate::abi::pon_const_int(i64::from(group)) };
				if object.is_null() {
					return std::ptr::null_mut();
				}
				objects.push(object);
			}
			return unsafe { crate::abi::seq::pon_build_list(objects.as_mut_ptr(), objects.len()) };
		}
		if capacity <= ngroups {
			return raise_errno(last_errno(), None);
		}
		ngroups = capacity;
	}
}

unsafe extern "C" fn os_setgroups(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.setgroups expected one argument");
	}
	let values = match super::builtins_batch::collect_iterable(args[0]) {
		Ok(values) => values,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	let mut groups = Vec::with_capacity(values.len());
	for value in values {
		let group = match int_arg(value, "setgroups group") {
			Ok(value) => value,
			Err(error) => return error,
		};
		groups.push(group as libc::gid_t);
	}
	if unsafe { libc::setgroups(groups.len() as _, groups.as_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}
unsafe extern "C" fn os_wcoredump(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WCOREDUMP") {
		Ok(status) => bool_object((status & 0x80) != 0),
		Err(error) => error,
	}
}

unsafe extern "C" fn os_sysconf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let name = match single_int_arg(argv, argc, "sysconf") {
		Ok(value) => value,
		Err(error) => return error,
	};
	sysconf_result(unsafe { libc::sysconf(name as libc::c_int) })
}

/// CPython's `conv_confname`: the integer itself, or a string key of the
/// module-level `*_names` map (`ValueError` on unknown names).
fn confname_arg(
	object: *mut PyObject,
	names: &[(&str, i32)],
) -> Result<libc::c_int, *mut PyObject> {
	// Tagged immediates are integers; only heap objects can be str.
	if !object.is_null() && !crate::tag::is_small_int(object) {
		// SAFETY: Heap pointer with a live header after the tag check.
		if let Some(text) = unsafe { crate::types::type_::unicode_text(object) } {
			return names
				.iter()
				.find(|(name, _)| *name == text)
				.map(|&(_, value)| value)
				.ok_or_else(|| {
					crate::abi::exc::raise_kind_error_text(
						ExceptionKind::ValueError,
						"unrecognized configuration name",
					)
				});
		}
	}
	int_arg(object, "confstr").map(|value| value as libc::c_int)
}

unsafe extern "C" fn os_confstr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.confstr expected one argument");
	}
	let name = match confname_arg(args[0], CONFSTR_NAMES) {
		Ok(value) => value,
		Err(error) => return error,
	};
	clear_errno();
	let size = unsafe { libc::confstr(name as libc::c_int, std::ptr::null_mut(), 0) };
	let errno = current_errno();
	if size == 0 && errno != 0 {
		return raise_errno(errno, None);
	}
	let mut buffer = vec![0u8; size.max(1)];
	let written =
		unsafe { libc::confstr(name as libc::c_int, buffer.as_mut_ptr().cast(), buffer.len()) };
	let errno = current_errno();
	if written == 0 && errno != 0 {
		return raise_errno(errno, None);
	}
	let slice = if written == 0 {
		&[][..]
	} else {
		&buffer[..written.saturating_sub(1)]
	};
	match std::str::from_utf8(slice) {
		Ok(text) => unsafe { pon_const_str(text.as_ptr(), text.len()) },
		Err(_) => crate::abi::exc::raise_kind_error_text(
			ExceptionKind::UnicodeDecodeError,
			"confstr result is not valid UTF-8",
		),
	}
}

unsafe extern "C" fn os_pathconf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.pathconf expected two arguments");
	}
	let path = match path_arg(args[0], "pathconf") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let name = match int_arg(args[1], "pathconf name") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	clear_errno();
	let value = unsafe { libc::pathconf(c_path.as_ptr(), name as libc::c_int) };
	sysconf_result(value)
}

unsafe extern "C" fn os_fpathconf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.fpathconf expected two arguments");
	}
	let fd = match int_arg(args[0], "fpathconf fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let name = match int_arg(args[1], "fpathconf name") {
		Ok(value) => value,
		Err(error) => return error,
	};
	clear_errno();
	let value = unsafe { libc::fpathconf(fd as libc::c_int, name as libc::c_int) };
	sysconf_result(value)
}

fn sysconf_result(value: libc::c_long) -> *mut PyObject {
	if value == -1 && current_errno() != 0 {
		return raise_errno(current_errno(), None);
	}
	unsafe { crate::abi::pon_const_int(value as i64) }
}

#[cfg(not(target_os = "macos"))]
use libc::__errno_location as errno_location;
#[cfg(target_os = "macos")]
use libc::__error as errno_location;

fn clear_errno() {
	// SAFETY: `errno_location` returns the calling thread's live errno slot.
	unsafe { *errno_location() = 0 };
}

fn current_errno() -> i32 {
	// SAFETY: `errno_location` returns the calling thread's live errno slot.
	unsafe { *errno_location() }
}

unsafe extern "C" fn os_device_encoding(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "device_encoding") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::isatty(fd) } == 0 {
		return unsafe { crate::abi::pon_none() };
	}
	let ptr = unsafe { libc::nl_langinfo(libc::CODESET) };
	if ptr.is_null() {
		return unsafe { crate::abi::pon_none() };
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn os_get_terminal_size(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() > 1 {
		return crate::abi::return_null_with_error(
			"os.get_terminal_size expected at most one argument",
		);
	}
	let fd = if let Some(arg) = args.first() {
		match int_arg(*arg, "get_terminal_size fd") {
			Ok(value) => value as libc::c_int,
			Err(error) => return error,
		}
	} else {
		libc::STDOUT_FILENO
	};
	let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
	if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	let size = unsafe { size.assume_init() };
	terminal_size_object(size.ws_col as i64, size.ws_row as i64)
}

fn terminal_size_object(columns: i64, lines: i64) -> *mut PyObject {
	let class = match terminal_size_class() {
		Ok(class) => class,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	let mut items =
		[unsafe { crate::abi::pon_const_int(columns) }, unsafe { crate::abi::pon_const_int(lines) }];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	let tuple = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
	if tuple.is_null() {
		return std::ptr::null_mut();
	}
	let mut argv = [tuple];
	unsafe { crate::abi::pon_call(class, argv.as_mut_ptr(), argv.len()) }
}

unsafe extern "C" fn os_lockf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.lockf expected three arguments");
	}
	let fd = match int_arg(args[0], "lockf fd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let cmd = match int_arg(args[1], "lockf cmd") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let len = match int_arg(args[2], "lockf len") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::lockf(fd as libc::c_int, cmd as libc::c_int, len as libc::off_t) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_login_tty(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let fd = match one_fd_arg(argv, argc, "login_tty") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { libc::login_tty(fd) } < 0 {
		return raise_errno(last_errno(), None);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn os_mknod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.is_empty() || args.len() > 4 {
		return crate::abi::return_null_with_error(
			"os.mknod expected path with optional mode and device",
		);
	}
	let path = match path_arg(args[0], "mknod") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match optional_arg(args, 1).map(|object| int_arg(object, "mknod mode")) {
		None => 0o600,
		Some(Ok(value)) => value,
		Some(Err(error)) => return error,
	};
	let device = match optional_arg(args, 2).map(|object| int_arg(object, "mknod device")) {
		None => 0,
		Some(Ok(value)) => value,
		Some(Err(error)) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 3, "mknod") {
		return error;
	}
	let c_path = match c_path(&path) {
		Ok(path) => path,
		Err(error) => return error,
	};
	if unsafe {
		libc::mknodat(libc::AT_FDCWD, c_path.as_ptr(), mode as libc::mode_t, device as libc::dev_t)
	} < 0
	{
		return raise_errno(last_errno(), Some(&path));
	}
	unsafe { crate::abi::pon_none() }
}

/// Borrows the argv slots as a slice; NULL argv reads as empty.
unsafe fn call_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
	if argv.is_null() || argc == 0 {
		&[]
	} else {
		// SAFETY: The caller passed `argc` live argument slots.
		unsafe { std::slice::from_raw_parts(argv, argc) }
	}
}

/// `int`-typed argument (bool included, like CPython's implicit acceptance).
fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	if crate::tag::is_small_int(object) {
		return Ok(crate::tag::untag_small_int(object));
	}
	// SAFETY: Non-immediate pointers are boxed objects; conversion type-checks.
	match unsafe { crate::types::int::to_bigint_including_bool(object) } {
		Some(value) => value.to_i64().ok_or_else(|| {
			crate::abi::exc::raise_kind_error_text(
				ExceptionKind::OverflowError,
				&format!("{what} is too large to fit in a C integer"),
			)
		}),
		None => Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("{what} must be an integer"),
		)),
	}
}

/// Path argument: str passes through, other objects defer to `__fspath__`
/// (so `pathlib.Path` works).  Divergence: CPython also accepts `bytes`
/// paths; pon's path surface is str-only and raises the fspath TypeError.
fn path_arg(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
	let raw = crate::tag::untag_arg(object);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		// SAFETY: Heap pointer with a live header after the tag checks.
		if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
			return Ok(text.to_owned());
		}
		// SAFETY: Live header per the checks above.
		let ty = unsafe { (*raw).ob_type.cast_mut() };
		let hook = unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) };
		if !hook.is_null() {
			let bound = unsafe { crate::descr::descriptor_get(hook, raw, ty) };
			if bound.is_null() {
				return Err(std::ptr::null_mut());
			}
			// SAFETY: Call helper follows the NULL-sentinel error contract.
			let result = unsafe { crate::abi::pon_call(bound, std::ptr::null_mut(), 0) };
			if result.is_null() {
				return Err(std::ptr::null_mut());
			}
			let result = crate::tag::untag_arg(result);
			if !result.is_null() && !crate::tag::is_small_int(result) {
				// SAFETY: Boxed pointer per the checks above.
				if let Some(text) = unsafe { crate::types::type_::unicode_text(result) } {
					return Ok(text.to_owned());
				}
			}
		}
	}
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		&format!("{what}: path should be a str or an os.PathLike object"),
	))
}

/// NUL-checked C path, matching CPython's embedded-NUL ValueError.
fn c_path(path: &str) -> Result<std::ffi::CString, *mut PyObject> {
	std::ffi::CString::new(path).map_err(|_| {
		crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "embedded null byte")
	})
}

/// Optional trailing argument: absent and None (the native keyword binder
/// fills absent slots with None) both read as "not supplied".
fn optional_arg(args: &[*mut PyObject], index: usize) -> Option<*mut PyObject> {
	let value = args.get(index).copied()?;
	if value.is_null() {
		return None;
	}
	let raw = crate::tag::untag_arg(value);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return Some(value);
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	if unsafe { crate::types::dict::type_name(raw) } == Some("NoneType") {
		return None;
	}
	Some(value)
}

/// Raises the CPython OSError subclass for `errno` (PEP 3151) with the
/// `[Errno N] strerror` message shape and optional filename context.
/// Shared with sibling fd-syscall modules (`fcntl`).
pub(crate) fn raise_errno(errno: i32, path: Option<&str>) -> *mut PyObject {
	match alloc_errno_exception(errno, path) {
		Ok(exception) => unsafe { crate::abi::exc::pon_raise(exception, std::ptr::null_mut()) },
		Err(error) => error,
	}
}

fn alloc_errno_exception(errno: i32, path: Option<&str>) -> Result<*mut PyObject, *mut PyObject> {
	let (kind, class_name) = errno_exception(errno);
	let detail = errno_detail(errno);
	let message = errno_message(errno, &detail, path);
	let errno_obj = unsafe { crate::abi::pon_const_int(i64::from(errno)) };
	if errno_obj.is_null() {
		return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
	}
	let detail_obj = unsafe { pon_const_str(detail.as_ptr(), detail.len()) };
	if detail_obj.is_null() {
		return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
	}
	let mut args = vec![errno_obj, detail_obj];
	if let Some(path) = path {
		let path_obj = unsafe { pon_const_str(path.as_ptr(), path.len()) };
		if path_obj.is_null() {
			return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
		}
		args.push(path_obj);
	}
	let Some(class) = crate::abi::runtime_global(intern(class_name)) else {
		return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
	};
	let exception =
		crate::abi::exc::alloc_exception_instance(class.cast::<crate::object::PyType>(), &args);
	if exception.is_null() {
		Err(std::ptr::null_mut())
	} else {
		Ok(exception)
	}
}

fn errno_exception(errno: i32) -> (ExceptionKind, &'static str) {
	match errno {
		libc::EEXIST => (ExceptionKind::FileExistsError, "FileExistsError"),
		libc::ENOENT => (ExceptionKind::FileNotFoundError, "FileNotFoundError"),
		libc::EISDIR => (ExceptionKind::IsADirectoryError, "IsADirectoryError"),
		libc::ENOTDIR => (ExceptionKind::NotADirectoryError, "NotADirectoryError"),
		libc::EACCES | libc::EPERM => (ExceptionKind::PermissionError, "PermissionError"),
		libc::EINTR => (ExceptionKind::InterruptedError, "InterruptedError"),
		libc::EPIPE => (ExceptionKind::BrokenPipeError, "BrokenPipeError"),
		libc::ECHILD => (ExceptionKind::ChildProcessError, "ChildProcessError"),
		libc::ESRCH => (ExceptionKind::ProcessLookupError, "ProcessLookupError"),
		// PEP 3151 groups the whole would-block family (non-blocking
		// connect reports EINPROGRESS/EALREADY) under BlockingIOError.
		libc::EAGAIN | libc::EINPROGRESS | libc::EALREADY => {
			(ExceptionKind::BlockingIOError, "BlockingIOError")
		},
		libc::ETIMEDOUT => (ExceptionKind::TimeoutError, "TimeoutError"),
		libc::ECONNABORTED => (ExceptionKind::ConnectionAbortedError, "ConnectionAbortedError"),
		libc::ECONNREFUSED => (ExceptionKind::ConnectionRefusedError, "ConnectionRefusedError"),
		libc::ECONNRESET => (ExceptionKind::ConnectionResetError, "ConnectionResetError"),
		_ => (ExceptionKind::OSError, "OSError"),
	}
}

fn errno_detail(errno: i32) -> String {
	unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
		.to_string_lossy()
		.into_owned()
}

fn errno_message(errno: i32, detail: &str, path: Option<&str>) -> String {
	match path {
		Some(path) => format!("[Errno {errno}] {detail}: '{path}'"),
		None => format!("[Errno {errno}] {detail}"),
	}
}

fn last_errno() -> i32 {
	std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO)
}

fn set_fd_cloexec(fd: libc::c_int, cloexec: bool) -> Result<(), i32> {
	// SAFETY: Plain fcntl query; the fd is validated by the kernel.
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
	if flags < 0 {
		return Err(last_errno());
	}
	let new_flags = if cloexec {
		flags | libc::FD_CLOEXEC
	} else {
		flags & !libc::FD_CLOEXEC
	};
	// SAFETY: Plain fcntl update; the fd is validated by the kernel.
	if unsafe { libc::fcntl(fd, libc::F_SETFD, new_flags) } < 0 {
		Err(last_errno())
	} else {
		Ok(())
	}
}

/// Honest refusal for the keyword-only fd-relative parameters: CPython
/// raises NotImplementedError when `dir_fd` is unavailable on a platform,
/// and pon's capability sets (`os.supports_dir_fd`) are empty.
fn reject_dir_fd(args: &[*mut PyObject], index: usize, what: &str) -> Result<(), *mut PyObject> {
	if optional_arg(args, index).is_none() {
		return Ok(());
	}
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::NotImplementedError,
		&format!("{what}: dir_fd unavailable on this platform"),
	))
}

/// `os.open(path, flags, mode=0o777, *, dir_fd=None)` over `open(2)`;
/// returns the raw fd as int.
unsafe extern "C" fn os_open(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(2..=4).contains(&args.len()) {
		let message = "os.open expected 2 to 3 arguments (path, flags, mode=0o777)";
		return crate::abi::return_null_with_error(message);
	}
	let path = match path_arg(args[0], "open") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let flags = match int_arg(args[1], "open flags") {
		Ok(flags) => flags,
		Err(error) => return error,
	};
	let mode = match optional_arg(args, 2).map(|object| int_arg(object, "open mode")) {
		None => 0o777,
		Some(Ok(mode)) => mode,
		Some(Err(error)) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 3, "open") {
		return error;
	}
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated; the variadic mode argument uses the
	// default-promoted c_uint width `open(2)` expects.
	let fd = unsafe { libc::open(c_path.as_ptr(), flags as libc::c_int, mode as libc::c_uint) };
	if fd < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(i64::from(fd)) }
}

/// `os.close(fd)` over `close(2)`.
unsafe extern "C" fn os_close(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.close expected one argument");
	}
	let fd = match int_arg(args[0], "close fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	// SAFETY: Plain syscall; the fd is validated by the kernel.
	if unsafe { libc::close(fd as libc::c_int) } < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.dup(fd)` over `dup(2)`, matching CPython's non-inheritable default.
unsafe extern "C" fn os_dup(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.dup expected one argument");
	}
	let fd = match int_arg(args[0], "dup fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	// SAFETY: Plain syscall; the fd is validated by the kernel.
	let duplicated = unsafe { libc::dup(fd as libc::c_int) };
	if duplicated < 0 {
		return raise_errno(last_errno(), None);
	}
	if let Err(errno) = set_fd_cloexec(duplicated, true) {
		// SAFETY: Best-effort cleanup for the just-created descriptor.
		unsafe { libc::close(duplicated) };
		return raise_errno(errno, None);
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(i64::from(duplicated)) }
}

/// `os.dup2(fd, fd2, inheritable=True)` over `dup2(2)`.
unsafe extern "C" fn os_dup2(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(2..=3).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.dup2 expected two or three arguments");
	}
	let fd = match int_arg(args[0], "dup2 fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let fd2 = match int_arg(args[1], "dup2 fd2") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let inheritable = if let Some(&value) = args.get(2) {
		match int_arg(value, "dup2 inheritable") {
			Ok(value) => value != 0,
			Err(error) => return error,
		}
	} else {
		true
	};
	// SAFETY: Plain syscall; descriptors are validated by the kernel.
	let duplicated = unsafe { libc::dup2(fd as libc::c_int, fd2 as libc::c_int) };
	if duplicated < 0 {
		return raise_errno(last_errno(), None);
	}
	if let Err(errno) = set_fd_cloexec(duplicated, !inheritable) {
		return raise_errno(errno, None);
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(i64::from(duplicated)) }
}

/// `os.fdopen(fd, ...)`: CPython's thin alias for `open(fd, ...)`.
unsafe extern "C" fn os_fdopen(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 {
		return crate::abi::return_null_with_error("os.fdopen expected at least one argument");
	}
	// SAFETY: Same argv/argc contract as builtin `open`.
	unsafe { super::io::builtin_open(argv, argc) }
}

/// `os.read(fd, n)` over `read(2)`: at most `n` bytes as a bytes object.
unsafe extern "C" fn os_read(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.read expected two arguments");
	}
	let fd = match int_arg(args[0], "read fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let size = match int_arg(args[1], "read size") {
		Ok(size) => size,
		Err(error) => return error,
	};
	if size < 0 {
		// CPython surfaces a negative length as EINVAL from the syscall layer.
		return raise_errno(libc::EINVAL, None);
	}
	let mut buffer = vec![0u8; size as usize];
	// SAFETY: `buffer` owns `size` writable bytes for the syscall to fill.
	let count = unsafe { libc::read(fd as libc::c_int, buffer.as_mut_ptr().cast(), buffer.len()) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: The syscall wrote `count` bytes; allocation copies them.
	unsafe { crate::abi::str_::pon_const_bytes(buffer.as_ptr(), count as usize) }
}
/// `os.readinto(fd, buffer)` over `read(2)`: fills a writable bytes-like
/// target in place and returns the byte count. `_pyio.FileIO.readinto`
/// dispatches here directly.
unsafe extern "C" fn os_readinto(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.readinto expected two arguments");
	}
	let fd = match int_arg(args[0], "readinto fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let target = crate::tag::untag_arg(args[1]);
	let (dst, dst_len) = match writable_bytes_target(target) {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	// SAFETY: `dst` addresses `dst_len` writable bytes for the syscall fill.
	let count = unsafe { libc::read(fd as libc::c_int, dst.cast(), dst_len) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(count as i64) }
}

/// `os.write(fd, data)` over `write(2)`: returns the byte count written.
unsafe extern "C" fn os_write(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.write expected two arguments");
	}
	let fd = match int_arg(args[0], "write fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let data = crate::tag::untag_arg(args[1]);
	let payload = match readable_bytes_payload(data) {
		Ok(payload) => payload,
		Err(error) => return error,
	};
	// SAFETY: `payload` borrows live object bytes for the syscall to read.
	let count = unsafe { libc::write(fd as libc::c_int, payload.as_ptr().cast(), payload.len()) };
	if count < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(count as i64) }
}

/// `os.lseek(fd, position, whence)` over `lseek(2)`: returns the resulting
/// offset.  The whence argument takes the `SEEK_*` constants above;
/// validation is the host's (EINVAL for junk whence/offset combinations),
/// exactly like CPython's thin wrapper.
unsafe extern "C" fn os_lseek(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return crate::abi::return_null_with_error("os.lseek expected three arguments");
	}
	let fd = match int_arg(args[0], "lseek fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let position = match int_arg(args[1], "lseek position") {
		Ok(position) => position,
		Err(error) => return error,
	};
	let whence = match int_arg(args[2], "lseek whence") {
		Ok(whence) => whence,
		Err(error) => return error,
	};
	// SAFETY: Plain fd syscall; failure reports through errno below.
	let offset = unsafe { libc::lseek(fd as libc::c_int, position, whence as libc::c_int) };
	if offset < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(offset) }
}

/// Borrows a bytes/bytearray payload; `None` for other types.
fn bytes_payload<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	let ty = unsafe { (*object).ob_type };
	if crate::types::bytes_::is_bytes_type(ty) {
		// SAFETY: The type check proved PyBytes layout.
		Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() })
	} else if crate::types::bytearray_::is_bytearray_type(ty) {
		// SAFETY: The type check proved PyByteArray layout.
		Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() })
	} else {
		None
	}
}

/// Borrows a readable bytes/bytearray/memoryview payload for `os.write`.
fn readable_bytes_payload<'a>(object: *mut PyObject) -> Result<&'a [u8], *mut PyObject> {
	if let Some(payload) = bytes_payload(object) {
		return Ok(payload);
	}
	if object.is_null() || crate::tag::is_small_int(object) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"a bytes-like object is required",
		));
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	let ty = unsafe { (*object).ob_type };
	if crate::types::memoryview::is_memoryview_type(ty) {
		let view = unsafe { &*object.cast::<crate::types::memoryview::PyMemoryView>() };
		if view.released {
			return Err(unsafe {
				crate::abi::exc::pon_raise_value_error(
					crate::types::memoryview::RELEASED_ERROR.as_ptr(),
					crate::types::memoryview::RELEASED_ERROR.len(),
				)
			});
		}
		// SAFETY: The live memoryview pins a contiguous byte window.
		return Ok(unsafe { view.as_slice() });
	}
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		"a bytes-like object is required",
	))
}

/// Borrows a writable bytearray/memoryview target for `os.readinto`.
fn writable_bytes_target(object: *mut PyObject) -> Result<(*mut u8, usize), *mut PyObject> {
	if object.is_null() {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"readinto() argument must be read-write bytes-like object, not 'NoneType'",
		));
	}
	if crate::tag::is_small_int(object) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"readinto() argument must be read-write bytes-like object, not int",
		));
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	let ty = unsafe { (*object).ob_type };
	if crate::types::bytearray_::is_bytearray_type(ty) {
		let bytearray = unsafe { &mut *object.cast::<crate::types::bytearray_::PyByteArray>() };
		return Ok((bytearray.bytes.as_mut_ptr(), bytearray.bytes.len()));
	}
	if crate::types::memoryview::is_memoryview_type(ty) {
		let view = unsafe { &mut *object.cast::<crate::types::memoryview::PyMemoryView>() };
		if view.released {
			return Err(unsafe {
				crate::abi::exc::pon_raise_value_error(
					crate::types::memoryview::RELEASED_ERROR.as_ptr(),
					crate::types::memoryview::RELEASED_ERROR.len(),
				)
			});
		}
		if view.readonly {
			return Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::TypeError,
				"readinto() argument must be read-write bytes-like object, not memoryview",
			));
		}
		return Ok((view.data, view.len));
	}
	let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		&format!("readinto() argument must be read-write bytes-like object, not {type_name}"),
	))
}

/// `os.unlink(path)` over `unlink(2)`.
unsafe extern "C" fn os_unlink(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.unlink expected one argument");
	}
	let path = match path_arg(args[0], "unlink") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::unlink(c_path.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.pipe()` over `pipe(2)`: returns the `(read_fd, write_fd)` pair.
unsafe extern "C" fn os_pipe(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	let mut fds = [0 as libc::c_int; 2];
	// SAFETY: `fds` is the 2-element array `pipe(2)` writes into.
	if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
		return raise_errno(last_errno(), None);
	}
	if let Err(errno) = set_fd_cloexec(fds[0], true).and_then(|()| set_fd_cloexec(fds[1], true)) {
		// SAFETY: Best-effort cleanup for the just-created descriptors.
		unsafe {
			libc::close(fds[0]);
			libc::close(fds[1]);
		}
		return raise_errno(errno, None);
	}
	// SAFETY: Singleton/boxing accessors follow the NULL-sentinel contract.
	let mut items = unsafe {
		[crate::abi::pon_const_int(i64::from(fds[0])), crate::abi::pon_const_int(i64::from(fds[1]))]
	};
	// SAFETY: `items` holds two live boxed ints.
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

struct MakedirsFailure {
	errno: i32,
	path:  String,
}

/// `os.makedirs(name, mode=0o777, exist_ok=False)`; creates missing parents
/// with the default directory mode and applies `mode` only to the leaf.
unsafe extern "C" fn os_makedirs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(1..=3).contains(&args.len()) {
		return crate::abi::return_null_with_error(
			"os.makedirs expected 1 to 3 arguments (name, mode=0o777, exist_ok=False)",
		);
	}
	let path = match path_arg(args[0], "makedirs") {
		Ok(path) => path,
		Err(error) => return error,
	};
	if let Err(error) = c_path(&path) {
		return error;
	}
	let mode = match optional_arg(args, 1).map(|object| int_arg(object, "makedirs mode")) {
		None => 0o777,
		Some(Ok(mode)) => mode,
		Some(Err(error)) => return error,
	};
	let exist_ok = match optional_arg(args, 2) {
		None => false,
		Some(object) => match unsafe { crate::abi::pon_is_true(object) } {
			0 => false,
			1 => true,
			_ => return std::ptr::null_mut(),
		},
	};
	match makedirs_impl(&path, mode as libc::mode_t, exist_ok) {
		Ok(()) => unsafe { crate::abi::pon_none() },
		Err(error) => raise_errno(error.errno, Some(error.path.as_str())),
	}
}

fn makedirs_impl(path: &str, mode: libc::mode_t, exist_ok: bool) -> Result<(), MakedirsFailure> {
	let (mut head, mut tail) = split_posix_path(path);
	if tail.is_empty() {
		let split = split_posix_path(&head);
		head = split.0;
		tail = split.1;
	}
	if !head.is_empty() && !tail.is_empty() && !path_exists(&head) {
		if let Err(error) = makedirs_impl(&head, 0o777 as libc::mode_t, exist_ok) {
			if error.errno != libc::EEXIST {
				return Err(error);
			}
		}
		if tail == "." {
			return Ok(());
		}
	}
	match mkdir_errno(path, mode) {
		Ok(()) => Ok(()),
		Err(_) if exist_ok && path_is_dir(path) => Ok(()),
		Err(errno) => Err(MakedirsFailure { errno, path: path.to_owned() }),
	}
}

fn split_posix_path(path: &str) -> (String, String) {
	let Some(last_sep) = path.rfind('/') else {
		return (String::new(), path.to_owned());
	};
	let split_at = last_sep + 1;
	let mut head = &path[..split_at];
	let tail = &path[split_at..];
	if !head.is_empty() && !head.bytes().all(|byte| byte == b'/') {
		head = head.trim_end_matches('/');
	}
	(head.to_owned(), tail.to_owned())
}

fn path_exists(path: &str) -> bool {
	std::fs::metadata(path).is_ok()
}

fn path_is_dir(path: &str) -> bool {
	std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn mkdir_errno(path: &str, mode: libc::mode_t) -> Result<(), i32> {
	let c_path = std::ffi::CString::new(path).expect("makedirs path was prechecked for NUL");
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::mkdir(c_path.as_ptr(), mode) } < 0 {
		Err(last_errno())
	} else {
		Ok(())
	}
}

/// `os.mkdir(path, mode=0o777, *, dir_fd=None)` over `mkdir(2)`; the mode is
/// masked by the process umask exactly like the syscall.
unsafe extern "C" fn os_mkdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(1..=3).contains(&args.len()) {
		let message = "os.mkdir expected 1 to 2 arguments (path, mode=0o777)";
		return crate::abi::return_null_with_error(message);
	}
	let path = match path_arg(args[0], "mkdir") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let mode = match optional_arg(args, 1).map(|object| int_arg(object, "mkdir mode")) {
		None => 0o777,
		Some(Ok(mode)) => mode,
		Some(Err(error)) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 2, "mkdir") {
		return error;
	}
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::mkdir(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.rmdir(path)` over `rmdir(2)`.
unsafe extern "C" fn os_rmdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.rmdir expected one argument");
	}
	let path = match path_arg(args[0], "rmdir") {
		Ok(path) => path,
		Err(error) => return error,
	};
	let c_path = match c_path(&path) {
		Ok(c_path) => c_path,
		Err(error) => return error,
	};
	// SAFETY: `c_path` is NUL-terminated.
	if unsafe { libc::rmdir(c_path.as_ptr()) } < 0 {
		return raise_errno(last_errno(), Some(&path));
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.lstat(path, *, dir_fd=None)` over `symlink_metadata` (never follows
/// the final symlink, exactly `lstat(2)`); `posixpath.lexists` catches the
/// OSError for missing paths.
unsafe extern "C" fn os_lstat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if !(1..=2).contains(&args.len()) {
		return crate::abi::return_null_with_error("os.lstat expected one argument");
	}
	let path = match path_arg(args[0], "lstat") {
		Ok(path) => path,
		Err(error) => return error,
	};
	if let Err(error) = reject_dir_fd(args, 1, "lstat") {
		return error;
	}
	match std::fs::symlink_metadata(&path) {
		Ok(metadata) => stat_result_object(&metadata),
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
	}
}

/// `os.scandir(path='.')`: lazy directory iterator yielding `os.DirEntry`
/// objects with CPython's context-manager and cached-stat surface.
unsafe extern "C" fn os_scandir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() > 1 {
		return crate::abi::return_null_with_error("os.scandir expected at most one argument");
	}
	let path = if args.first().copied().is_none_or(is_none_value) {
		".".to_owned()
	} else {
		match path_arg(args[0], "scandir") {
			Ok(path) => path,
			Err(error) => return error,
		}
	};
	if let Err(error) = c_path(&path) {
		return error;
	}
	match std::fs::read_dir(&path) {
		Ok(entries) => alloc_scandir_iterator(path, entries),
		Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
	}
}

/// `os.strerror(errno)`: host strerror table exposed as a Python string.
unsafe extern "C" fn os_strerror(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return crate::abi::return_null_with_error("os.strerror expected one argument");
	}
	let errno = match int_arg(args[0], "strerror code") {
		Ok(errno) => errno,
		Err(error) => return error,
	};
	// SAFETY: `strerror` returns a NUL-terminated static message.
	let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno as libc::c_int)) }
		.to_string_lossy()
		.into_owned();
	// SAFETY: String allocation helper follows the NULL-sentinel contract.
	unsafe { pon_const_str(detail.as_ptr(), detail.len()) }
}

/// Single int `status` word shared by the wait-status inspectors.
fn status_arg(
	argv: *mut *mut PyObject,
	argc: usize,
	what: &str,
) -> Result<libc::c_int, *mut PyObject> {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return Err(crate::abi::return_null_with_error(format!("os.{what} expected one argument")));
	}
	int_arg(args[0], what).map(|status| status as libc::c_int)
}

/// `os.waitpid(pid, options)` over `waitpid(2)`: `(pid, status)` tuple.
/// With nothing to reap the host answers ECHILD, surfaced as CPython's
/// ChildProcessError — exactly what `subprocess.Popen.__del__`'s reaper and
/// asyncio's child watchers catch on their no-child paths.
unsafe extern "C" fn os_waitpid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("os.waitpid expected two arguments");
	}
	let pid = match int_arg(args[0], "waitpid pid") {
		Ok(pid) => pid,
		Err(error) => return error,
	};
	let options = match int_arg(args[1], "waitpid options") {
		Ok(options) => options,
		Err(error) => return error,
	};
	let mut status: libc::c_int = 0;
	// SAFETY: `status` is a live out-slot for the syscall to fill.
	let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, options as libc::c_int) };
	if reaped < 0 {
		return raise_errno(last_errno(), None);
	}
	// SAFETY: Integer boxing helpers follow the NULL-sentinel error contract.
	let mut items = [unsafe { crate::abi::pon_const_int(i64::from(reaped)) }, unsafe {
		crate::abi::pon_const_int(i64::from(status))
	}];
	if items.iter().any(|item| item.is_null()) {
		return std::ptr::null_mut();
	}
	// SAFETY: `items` is a live window for the duration of the call.
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

/// `os.WIFEXITED(status)`: true when the child exited normally.
unsafe extern "C" fn os_wifexited(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WIFEXITED") {
		Ok(status) => bool_object(libc::WIFEXITED(status)),
		Err(error) => error,
	}
}

/// `os.WEXITSTATUS(status)`: the low 8-bit exit status from a normal exit.
unsafe extern "C" fn os_wexitstatus(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WEXITSTATUS") {
		Ok(status) => {
			// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
			unsafe { crate::abi::pon_const_int(i64::from(libc::WEXITSTATUS(status))) }
		},
		Err(error) => error,
	}
}

/// `os.WIFSIGNALED(status)`: true when the child was terminated by a signal.
unsafe extern "C" fn os_wifsignaled(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WIFSIGNALED") {
		Ok(status) => bool_object(libc::WIFSIGNALED(status)),
		Err(error) => error,
	}
}

/// `os.WTERMSIG(status)`: the signal that terminated the child.
unsafe extern "C" fn os_wtermsig(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WTERMSIG") {
		Ok(status) => {
			// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
			unsafe { crate::abi::pon_const_int(i64::from(libc::WTERMSIG(status))) }
		},
		Err(error) => error,
	}
}

/// `os.WIFCONTINUED(status)`: true when the child resumed after job-control
/// stop.
unsafe extern "C" fn os_wifcontinued(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WIFCONTINUED") {
		Ok(status) => bool_object(libc::WIFCONTINUED(status)),
		Err(error) => error,
	}
}

/// `os.waitstatus_to_exitcode(status)`: pure status-word math, exactly
/// CPython's `os_waitstatus_to_exitcode_impl` — `WEXITSTATUS` for a normal
/// exit, `-WTERMSIG` for a signal death, ValueError for stopped/invalid
/// words.  `subprocess._handle_exitstatus` calls it on every reaped status.
unsafe extern "C" fn os_waitstatus_to_exitcode(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let status = match status_arg(argv, argc, "waitstatus_to_exitcode") {
		Ok(status) => status,
		Err(error) => return error,
	};
	let exitcode = if libc::WIFEXITED(status) {
		i64::from(libc::WEXITSTATUS(status))
	} else if libc::WIFSIGNALED(status) {
		-i64::from(libc::WTERMSIG(status))
	} else if libc::WIFSTOPPED(status) {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			&format!("process stopped by delivery of signal {}", libc::WSTOPSIG(status)),
		);
	} else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			&format!("invalid wait status: {status}"),
		);
	};
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	unsafe { crate::abi::pon_const_int(exitcode) }
}

/// `os.WIFSTOPPED(status)`: true when the word reports a stopped child.
/// `subprocess._del_safe` binds it at import time for the `__del__` reaper.
unsafe extern "C" fn os_wifstopped(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WIFSTOPPED") {
		Ok(status) => bool_object(libc::WIFSTOPPED(status)),
		Err(error) => error,
	}
}

/// `os.WSTOPSIG(status)`: the signal that stopped the child (import-time
/// `subprocess._del_safe` binding, read next to `WIFSTOPPED`).
unsafe extern "C" fn os_wstopsig(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match status_arg(argv, argc, "WSTOPSIG") {
		Ok(status) => {
			// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
			unsafe { crate::abi::pon_const_int(i64::from(libc::WSTOPSIG(status))) }
		},
		Err(error) => error,
	}
}

// ---------------------------------------------------------------------------
// os.PathLike
//
// CPython defines `PathLike` in `os.py` as an ABC (metaclass `abc.ABCMeta`)
// whose `__subclasshook__` answers "does the class implement `__fspath__`?".
// The native seed cannot construct it through the `abc` module: `os` is one
// of the frozen EAGER_MODULES, registered during runtime init before any
// source module (like the vendored `abc.py`) can be imported.  The same
// contract is served structurally instead: a dedicated metaclass heap type
// carries native `__instancecheck__`/`__subclasscheck__` hooks probing
// `__fspath__` on the candidate's MRO — `descr::isinstance`/`issubclass`
// dispatch through exactly those metaclass hooks, the same path a
// Python-level ABCMeta override takes.  Both classes are built by
// `build_class_from_namespace`, the machinery behind `class` statements.
//
// Documented divergences from CPython:
// * `type(os.PathLike)` is the private `os._PathLikeMeta`, not `abc.ABCMeta`,
//   and the ABC registry API (`PathLike.register`) does not exist.
// * Instantiating `os.PathLike()` is not blocked (no abstractmethod machinery);
//   CPython raises TypeError.  Calling the inherited `__fspath__` raises
//   NotImplementedError like CPython's abstract body.

fn pathlike_class() -> Result<*mut PyObject, String> {
	static CLASS: std::sync::LazyLock<Result<usize, String>> =
		std::sync::LazyLock::new(|| build_pathlike_class().map(|class| class as usize));
	CLASS.clone().map(|class| class as *mut PyObject)
}

fn class_str_attr(
	namespace: *mut crate::types::type_::PyClassDict,
	name: &str,
	value: &str,
) -> Result<(), String> {
	// SAFETY: String allocation helper; NULL is checked below.
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	if object.is_null() {
		return Err(format!("failed to allocate os.PathLike attribute '{name}'"));
	}
	// SAFETY: The caller passes a live namespace box.
	unsafe { (&mut *namespace).set(intern(name), object) };
	Ok(())
}

fn class_function_attr(
	namespace: *mut crate::types::type_::PyClassDict,
	name: &str,
	entry: BuiltinFn,
) -> Result<(), String> {
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let function = unsafe {
		crate::abi::pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern(name),
		)
	};
	if function.is_null() {
		return Err(format!("failed to allocate os.PathLike method '{name}'"));
	}
	// SAFETY: The caller passes a live namespace box.
	unsafe { (&mut *namespace).set(intern(name), function) };
	Ok(())
}

fn build_pathlike_class() -> Result<*mut PyObject, String> {
	let type_type = crate::abi::runtime_type_type();
	if type_type.is_null() {
		return Err("builtin 'type' is not initialized for os.PathLike".to_owned());
	}
	let meta_namespace = crate::types::type_::new_namespace();
	if meta_namespace.is_null() {
		return Err("failed to allocate the os._PathLikeMeta namespace".to_owned());
	}
	class_str_attr(meta_namespace, "__module__", "os")?;
	class_function_attr(meta_namespace, "__instancecheck__", pathlike_instancecheck)?;
	class_function_attr(meta_namespace, "__subclasscheck__", pathlike_subclasscheck)?;
	class_function_attr(meta_namespace, "__getitem__", pathlike_class_getitem)?;
	class_function_attr(meta_namespace, "register", pathlike_register)?;
	// SAFETY: The base is the live builtin `type` object.
	let meta = unsafe {
		crate::types::type_::build_class_from_namespace(
			"_PathLikeMeta",
			&[type_type.cast::<PyObject>()],
			meta_namespace,
			&[],
		)
	};
	let meta = finish_class(meta, "_PathLikeMeta", type_type)?;

	let namespace = crate::types::type_::new_namespace();
	if namespace.is_null() {
		return Err("failed to allocate the os.PathLike namespace".to_owned());
	}
	class_str_attr(namespace, "__module__", "os")?;
	class_str_attr(
		namespace,
		"__doc__",
		"Abstract base class for implementing the file system path protocol.",
	)?;
	class_function_attr(namespace, "__fspath__", pathlike_fspath_abstract)?;
	class_function_attr(namespace, "__class_getitem__", pathlike_class_getitem)?;
	let keywords = [crate::types::type_::ClassKeyword { name: intern("metaclass"), value: meta }];
	// SAFETY: Implicit `object` base; the metaclass keyword is a live class.
	let class = unsafe {
		crate::types::type_::build_class_from_namespace("PathLike", &[], namespace, &keywords)
	};
	finish_class(class, "PathLike", meta.cast::<crate::object::PyType>())
}

/// Shared post-construction checks: surface the pending diagnostic as a
/// module-creation error and mirror `pon_build_class`'s ob_type fix-up.
fn finish_class(
	class: *mut PyObject,
	name: &str,
	metaclass: *mut crate::object::PyType,
) -> Result<*mut PyObject, String> {
	if class.is_null() {
		let detail =
			crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
		crate::thread_state::pon_err_clear();
		return Err(format!("failed to create os.{name}: {detail}"));
	}
	// SAFETY: Freshly built class object owned by this module build.
	unsafe {
		if (*class).ob_type.is_null() {
			(*class).ob_type = metaclass.cast_const();
		}
	}
	Ok(class)
}

/// True when `object`'s type carries `__fspath__` anywhere on its MRO.
fn implements_fspath(object: *mut PyObject) -> bool {
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return false;
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	let ty = unsafe { (*raw).ob_type.cast_mut() };
	!unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) }.is_null()
}

/// True when the class object `candidate` defines `__fspath__` on its MRO.
fn class_implements_fspath(candidate: *mut PyObject) -> bool {
	if candidate.is_null() || crate::tag::is_small_int(candidate) {
		return false;
	}
	!unsafe {
		crate::descr::lookup_in_type(candidate.cast::<crate::object::PyType>(), intern("__fspath__"))
	}
	.is_null()
}

fn bool_object(value: bool) -> *mut PyObject {
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_const_bool(i32::from(value)) }
}

/// `_PathLikeMeta.__instancecheck__(cls, instance)`: MRO subtype first, then
/// the `__fspath__` structural probe — but the probe only answers for
/// `PathLike` itself, mirroring `PathLike.__subclasshook__`'s
/// `if cls is PathLike` guard (subclasses get plain MRO semantics).
unsafe extern "C" fn pathlike_instancecheck(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("__instancecheck__ expected (cls, instance)");
	}
	let cls = args[0];
	let object = crate::tag::untag_arg(args[1]);
	if object.is_null() {
		return bool_object(false);
	}
	if !crate::tag::is_small_int(object) {
		// SAFETY: Heap pointer with a live header after the tag checks.
		let ty = unsafe { (*object).ob_type.cast_mut() };
		if unsafe { crate::mro::is_subtype(ty, cls.cast::<crate::object::PyType>()) } {
			return bool_object(true);
		}
	}
	let receiver_is_pathlike = pathlike_class().is_ok_and(|pathlike| pathlike == cls);
	bool_object(
		receiver_is_pathlike && (implements_fspath(args[1]) || instance_is_registered(args[1])),
	)
}

/// `_PathLikeMeta.__subclasscheck__(cls, candidate)`: see
/// [`pathlike_instancecheck`] for the `cls is PathLike` guard rationale.
unsafe extern "C" fn pathlike_subclasscheck(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("__subclasscheck__ expected (cls, candidate)");
	}
	let cls = args[0];
	let candidate = crate::tag::untag_arg(args[1]);
	if candidate.is_null() || crate::tag::is_small_int(candidate) {
		return bool_object(false);
	}
	// SAFETY: `issubclass` validated both operands as classes before
	// dispatching to this hook.
	if unsafe {
		crate::mro::is_subtype(
			candidate.cast::<crate::object::PyType>(),
			cls.cast::<crate::object::PyType>(),
		)
	} {
		return bool_object(true);
	}
	let receiver_is_pathlike = pathlike_class().is_ok_and(|pathlike| pathlike == cls);
	bool_object(
		receiver_is_pathlike
			&& (class_implements_fspath(candidate) || class_is_registered(candidate)),
	)
}

/// ABC registry backing `PathLike.register` (`pathlib` registers `PurePath`).
/// Registered classes are process-lifetime class objects, stored as raw
/// addresses; the checks walk `is_subtype` against every entry, matching
/// ABCMeta's registry semantics minus the negative cache.
static PATHLIKE_REGISTRY: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// `_PathLikeMeta.register(cls, subclass)`: records a virtual subclass and
/// returns it (CPython's decorator contract).
unsafe extern "C" fn pathlike_register(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return crate::abi::return_null_with_error("register expected one argument (a class)");
	}
	let subclass = crate::tag::untag_arg(args[1]);
	let is_class = !subclass.is_null() && !crate::tag::is_small_int(subclass) && {
		// SAFETY: Heap pointer with a live header after the tag checks; a
		// class object's own type linearizes over the builtin `type`.
		let meta = unsafe { (*subclass).ob_type.cast_mut() };
		unsafe { crate::mro::is_subtype(meta, crate::abi::runtime_type_type()) }
	};
	if !is_class {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"Can only register classes",
		);
	}
	let mut registry = PATHLIKE_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let entry = subclass as usize;
	if !registry.contains(&entry) {
		registry.push(entry);
	}
	drop(registry);
	args[1]
}

/// True when `object`'s type derives a `PathLike.register`ed class.
fn instance_is_registered(object: *mut PyObject) -> bool {
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return false;
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	class_is_registered(unsafe { (*raw).ob_type.cast_mut() }.cast::<PyObject>())
}

/// True when the class object `candidate` derives a registered class.
fn class_is_registered(candidate: *mut PyObject) -> bool {
	let registry = PATHLIKE_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	registry.iter().any(|&registered| {
		// SAFETY: Registry entries are live process-lifetime class objects.
		unsafe {
			crate::mro::is_subtype(
				candidate.cast::<crate::object::PyType>(),
				registered as *mut crate::object::PyType,
			)
		}
	})
}

/// `PathLike[str]`: served both as `_PathLikeMeta.__getitem__` (the subscript
/// dispatch path for class receivers) and as `PathLike.__class_getitem__`
/// (CPython publishes `classmethod(GenericAlias)` under that name).
unsafe extern "C" fn pathlike_class_getitem(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	let (origin, key) = match args.len() {
		// Unbound `PathLike.__class_getitem__(item)` call shape.
		1 => match pathlike_class() {
			Ok(class) => (class, args[0]),
			Err(message) => return crate::abi::return_null_with_error(message),
		},
		2 => (args[0], args[1]),
		_ => return crate::abi::return_null_with_error("__class_getitem__ expected one argument"),
	};
	let key = crate::tag::untag_arg(key);
	if key.is_null() {
		return std::ptr::null_mut();
	}
	let key_is_tuple = !crate::tag::is_small_int(key)
        // SAFETY: Heap pointer with a live header after the tag checks.
        && unsafe { crate::types::dict::type_name(key) } == Some("tuple");
	let key_args = if key_is_tuple {
		// SAFETY: The type check proved PyTuple layout.
		unsafe { (*key.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec()
	} else {
		vec![key]
	};
	crate::types::typealias::new_generic_alias(origin, key_args)
}

/// `PathLike.__fspath__` abstract body: CPython's `raise NotImplementedError`.
unsafe extern "C" fn pathlike_fspath_abstract(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::NotImplementedError, "")
}

// ---------------------------------------------------------------------------
// os.terminal_size
//
// CPython's `os.terminal_size` is a structseq (a tuple subclass with named
// fields) defined by the C `posix` module; `shutil.get_terminal_size`'s
// final fallback constructs one from a 2-int sequence and `argparse` reads
// `.columns`.  pon builds the same shape through the tuple-embedding heap
// class machinery (`class terminal_size(tuple)` with `columns`/`lines`
// properties and the CPython repr).  `os.get_terminal_size` is DELIBERATELY
// absent: `shutil` catches the AttributeError and takes its deterministic
// `(80, 24)`-shaped env fallback, which keeps differential runs stable
// whether or not a real tty is attached.

fn terminal_size_class() -> Result<*mut PyObject, String> {
	static CLASS: std::sync::LazyLock<Result<usize, String>> =
		std::sync::LazyLock::new(|| build_terminal_size_class().map(|class| class as usize));
	CLASS.clone().map(|class| class as *mut PyObject)
}

fn build_terminal_size_class() -> Result<*mut PyObject, String> {
	// SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
	let tuple_class = unsafe { crate::abi::pon_load_global(intern("tuple"), std::ptr::null_mut()) };
	if tuple_class.is_null() {
		crate::thread_state::pon_err_clear();
		return Err("builtin 'tuple' is not registered for os.terminal_size".to_owned());
	}
	// SAFETY: Same contract for the builtin `property` constructor.
	let property_class =
		unsafe { crate::abi::pon_load_global(intern("property"), std::ptr::null_mut()) };
	if property_class.is_null() {
		crate::thread_state::pon_err_clear();
		return Err("builtin 'property' is not registered for os.terminal_size".to_owned());
	}
	let namespace = crate::types::type_::new_namespace();
	if namespace.is_null() {
		return Err("failed to allocate the os.terminal_size namespace".to_owned());
	}
	class_str_attr(namespace, "__module__", "os")?;
	class_str_attr(
		namespace,
		"__doc__",
		"A tuple of (columns, lines) for holding terminal window size",
	)?;
	class_function_attr(namespace, "__repr__", terminal_size_repr)?;
	for (name, entry) in
		[("columns", terminal_size_columns as BuiltinFn), ("lines", terminal_size_lines as BuiltinFn)]
	{
		// SAFETY: Live builtin entry point with the runtime calling convention.
		let fget = unsafe { crate::abi::pon_make_function(entry as *const u8, 1, intern(name)) };
		if fget.is_null() {
			return Err(format!("failed to allocate os.terminal_size.{name} getter"));
		}
		let mut argv = [fget];
		// SAFETY: The builtin `property` class is callable with one fget slot.
		let descriptor =
			unsafe { crate::abi::pon_call(property_class, argv.as_mut_ptr(), argv.len()) };
		if descriptor.is_null() {
			let detail =
				crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
			crate::thread_state::pon_err_clear();
			return Err(format!("failed to build os.terminal_size.{name} property: {detail}"));
		}
		// SAFETY: `new_namespace` returned a live namespace box.
		unsafe { (&mut *namespace).set(intern(name), descriptor) };
	}
	// SAFETY: The base is the live builtin `tuple` class object.
	let class = unsafe {
		crate::types::type_::build_class_from_namespace(
			"terminal_size",
			&[tuple_class],
			namespace,
			&[],
		)
	};
	finish_class(class, "terminal_size", crate::abi::runtime_type_type())
}

/// Reads element `index` of a terminal_size receiver as an i64.
fn terminal_size_element(
	args: &[*mut PyObject],
	index: i64,
	what: &str,
) -> Result<i64, *mut PyObject> {
	if args.len() != 1 {
		return Err(crate::abi::return_null_with_error(format!("{what} expected only a receiver")));
	}
	// SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
	let key = unsafe { crate::abi::pon_const_int(index) };
	if key.is_null() {
		return Err(std::ptr::null_mut());
	}
	// SAFETY: Subscript dispatch resolves the tuple-embedded layout.
	let element = unsafe { crate::abstract_op::subscript_get(args[0], key) };
	if element.is_null() {
		return Err(std::ptr::null_mut());
	}
	int_arg(element, what)
}

/// `terminal_size.columns` property getter: `self[0]`.
unsafe extern "C" fn terminal_size_columns(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	match terminal_size_element(args, 0, "terminal_size.columns") {
		// SAFETY: Integer boxing helper follows the NULL-sentinel contract.
		Ok(value) => unsafe { crate::abi::pon_const_int(value) },
		Err(error) => error,
	}
}

/// `terminal_size.lines` property getter: `self[1]`.
unsafe extern "C" fn terminal_size_lines(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	match terminal_size_element(args, 1, "terminal_size.lines") {
		// SAFETY: Integer boxing helper follows the NULL-sentinel contract.
		Ok(value) => unsafe { crate::abi::pon_const_int(value) },
		Err(error) => error,
	}
}

/// CPython's structseq repr: `os.terminal_size(columns=80, lines=24)`.
unsafe extern "C" fn terminal_size_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	let columns = match terminal_size_element(args, 0, "terminal_size.columns") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let lines = match terminal_size_element(args, 1, "terminal_size.lines") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let text = format!("os.terminal_size(columns={columns}, lines={lines})");
	// SAFETY: String allocation helper follows the NULL-sentinel contract.
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

// ---------------------------------------------------------------------------
// Environment write-through: `putenv`/`unsetenv` (C `posix` pair, shared with
// the `posix` module like the rest of the syscall table) and the `os.py`-level
// `getenv`.  See [`environ_mapping`] for the live mapping contract.

/// str/bytes/PathLike argument for the environment write-through pair:
/// `os.fspath` coercion first (with its exact CPython TypeError), then the byte
/// payload (str as UTF-8, bytes raw).
unsafe fn env_bytes_arg(slot: *mut PyObject, what: &str) -> Result<Vec<u8>, *mut PyObject> {
	let mut argv = [slot];
	// SAFETY: One live argument slot built above.
	let coerced = unsafe { os_fspath(argv.as_mut_ptr(), 1) };
	if coerced.is_null() {
		return Err(std::ptr::null_mut());
	}
	let raw = crate::tag::untag_arg(coerced);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		// SAFETY: Heap pointer with a live header after the tag checks.
		if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
			return Ok(text.as_bytes().to_vec());
		}
		// SAFETY: Live header per the checks above.
		if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
			// SAFETY: The type check proved PyBytes layout.
			return Ok(unsafe { (*raw.cast::<crate::types::bytes_::PyBytes>()).as_slice() }.to_vec());
		}
	}
	Err(fs_codec_hook_type_error(what, raw))
}

/// `os.putenv(name, value)` / `posix.putenv`: writes through to the real process
/// environment and keeps cached `os.environ`/`posix.environ` dict-layout
/// bindings coherent with the new value.
unsafe extern "C" fn os_putenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return crate::abi::return_null_with_error(format!(
			"putenv expected 2 arguments, got {argc}"
		));
	}
	// SAFETY: Two live argument slots per the check above.
	let name = match unsafe { env_bytes_arg(*argv, "putenv") } {
		Ok(bytes) => bytes,
		Err(raised) => return raised,
	};
	// SAFETY: As above.
	let value = match unsafe { env_bytes_arg(*argv.add(1), "putenv") } {
		Ok(bytes) => bytes,
		Err(raised) => return raised,
	};
	if let Err(raised) = env_set_bytes(&name, &value) {
		return raised;
	}
	let key_obj = match env_object_from_bytes(&name, "os.environ key") {
		Ok(object) => object,
		Err(raised) => return raised,
	};
	let value_obj = match env_object_from_bytes(&value, "os.environ value") {
		Ok(object) => object,
		Err(raised) => return raised,
	};
	if let Err(raised) = sync_environ_bindings_set(key_obj, value_obj, std::ptr::null_mut()) {
		return raised;
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.unsetenv(name)` / `posix.unsetenv`: removes `name` from the real process
/// environment and removes the key from cached `os.environ`/`posix.environ`
/// dict-layout bindings.
unsafe extern "C" fn os_unsetenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::return_null_with_error(format!(
			"unsetenv expected 1 argument, got {argc}"
		));
	}
	// SAFETY: One live argument slot per the check above.
	let name = match unsafe { env_bytes_arg(*argv, "unsetenv") } {
		Ok(bytes) => bytes,
		Err(raised) => return raised,
	};
	if let Err(raised) = env_unset_bytes(&name) {
		return raised;
	}
	let key_obj = match env_object_from_bytes(&name, "os.environ key") {
		Ok(object) => object,
		Err(raised) => return raised,
	};
	if let Err(raised) = sync_environ_bindings_unset(key_obj, std::ptr::null_mut()) {
		return raised;
	}
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `os.get_exec_path(env=None)`: returns the PATH search directories used
/// by `subprocess` when the executable name has no directory component.
unsafe extern "C" fn os_get_exec_path(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Live argument slots per the runtime calling convention.
	let args = unsafe { call_args(argv, argc) };
	if args.len() > 1 {
		return crate::abi::return_null_with_error("get_exec_path() takes at most 1 argument");
	}
	let path = if args.first().copied().is_none_or(is_none_value) {
		std::env::var("PATH").ok()
	} else {
		match env_mapping_path(args[0]) {
			Ok(path) => path,
			Err(error) => return error,
		}
	}
	.unwrap_or_else(default_exec_path);
	super::builtins_batch::build_str_list(path.split(':').map(str::to_owned).collect())
}

fn default_exec_path() -> String {
	if cfg!(windows) {
		".;C:\\\\bin".to_owned()
	} else {
		"/bin:/usr/bin".to_owned()
	}
}

fn env_mapping_path(env: *mut PyObject) -> Result<Option<String>, *mut PyObject> {
	let key = unsafe { pon_const_str(b"PATH".as_ptr(), 4) };
	if key.is_null() {
		return Err(std::ptr::null_mut());
	}
	let default = unsafe { crate::abi::pon_none() };
	if default.is_null() {
		return Err(std::ptr::null_mut());
	}
	// SAFETY: Live mapping object; missing/failing `.get` propagates like
	// `os.py`'s `env.get('PATH')` expression.
	let get = unsafe { crate::abi::pon_get_attr(env, intern("get"), std::ptr::null_mut()) };
	if get.is_null() {
		return Err(std::ptr::null_mut());
	}
	let mut call_argv = [key, default];
	// SAFETY: Live bound method and two live argument slots.
	let value = unsafe { crate::abi::pon_call(get, call_argv.as_mut_ptr(), call_argv.len()) };
	if value.is_null() {
		return Err(std::ptr::null_mut());
	}
	path_string_from_value(value).map(Some)
}

fn path_string_from_value(value: *mut PyObject) -> Result<String, *mut PyObject> {
	if is_none_value(value) {
		return Ok(default_exec_path());
	}
	let raw = crate::tag::untag_arg(value);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"PATH must be str or bytes",
		));
	}
	// SAFETY: Heap pointer with a live header after the checks above.
	if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
		return Ok(text.to_owned());
	}
	if let Some(payload) = bytes_payload(raw) {
		return match std::str::from_utf8(payload) {
			Ok(text) => Ok(text.to_owned()),
			Err(_) => Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::UnicodeDecodeError,
				"PATH bytes are not valid UTF-8",
			)),
		};
	}
	Err(crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		"PATH must be str or bytes",
	))
}

fn is_none_value(object: *mut PyObject) -> bool {
	if object.is_null() {
		return true;
	}
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() || crate::tag::is_small_int(raw) {
		return false;
	}
	// SAFETY: Heap pointer with a live header after the checks above.
	unsafe { crate::types::dict::type_name(raw) == Some("NoneType") }
}

/// `os.getenv(key, default=None)`: `os.py`'s Python-level helper, served
/// natively.  Reads the LIVE `os.environ` module binding — rebinding
/// `os.environ`, as `test.support.os_helper.EnvironmentVarGuard.__exit__`
/// does, changes what getenv consults, exactly like the os.py module-global
/// read — then defers to `environ.get(key, default)` through attribute
/// dispatch so any mapping works.  The key must be str, matching
/// `_Environ.encodekey`'s check (a plain dict `.get` would silently return
/// the default).
unsafe extern "C" fn os_getenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		let message = "getenv() missing 1 required positional argument: 'key'";
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	if argc > 2 {
		let message =
			format!("getenv() takes from 1 to 2 positional arguments but {argc} were given");
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	// SAFETY: `argc` live argument slots per the checks above.
	let key = unsafe { *argv };
	let raw_key = crate::tag::untag_arg(key);
	if raw_key.is_null() {
		return std::ptr::null_mut();
	}
	// SAFETY: Heap-or-NULL after `untag_arg`; NULL was handled above.
	if crate::tag::is_small_int(raw_key)
		|| unsafe { crate::types::type_::unicode_text(raw_key) }.is_none()
	{
		let display = if crate::tag::is_small_int(raw_key) {
			"int"
		} else {
			// SAFETY: Heap pointer with a live header after the tag checks.
			unsafe { crate::types::dict::type_name(raw_key) }.unwrap_or("object")
		};
		let message = format!("str expected, not {display}");
		// SAFETY: Typed raise helper.
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	let default = if argc == 2 {
		// SAFETY: Two live argument slots per the argc check.
		unsafe { *argv.add(1) }
	} else {
		// SAFETY: Singleton accessor.
		unsafe { crate::abi::pon_none() }
	};
	let Some(environ) = crate::import::module_attr(intern("os"), intern("environ")) else {
		// `del os.environ` leaves getenv reading a missing global — the
		// exact failure os.py's `environ.get` produces.
		let message = "name 'environ' is not defined";
		return crate::abi::exc::raise_kind_error_text(ExceptionKind::NameError, message);
	};
	// SAFETY: Live environ binding; a missing or failing `get` attribute
	// propagates its own AttributeError, exactly like os.py's
	// `environ.get(key, default)` expression.
	let get = unsafe { crate::abi::pon_get_attr(environ, intern("get"), std::ptr::null_mut()) };
	if get.is_null() {
		return std::ptr::null_mut();
	}
	let mut call_argv = [key, default];
	// SAFETY: Live bound method and two live argument slots.
	unsafe { crate::abi::pon_call(get, call_argv.as_mut_ptr(), call_argv.len()) }
}
unsafe extern "C" fn os_getenvb(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	use std::os::unix::ffi::{OsStrExt, OsStringExt};

	if argc == 0 || argv.is_null() {
		let message = "getenvb() missing 1 required positional argument: 'key'";
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	if argc > 2 {
		let message =
			format!("getenvb() takes from 1 to 2 positional arguments but {argc} were given");
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	let key = unsafe { *argv };
	let raw_key = crate::tag::untag_arg(key);
	if raw_key.is_null() {
		return std::ptr::null_mut();
	}
	let key_bytes = match bytes_payload(raw_key) {
		Some(bytes) => bytes,
		None => {
			let display = if crate::tag::is_small_int(raw_key) {
				"int"
			} else {
				unsafe { crate::types::dict::type_name(raw_key) }.unwrap_or("object")
			};
			let message = format!("bytes expected, not {display}");
			return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		},
	};
	let default = if argc == 2 {
		unsafe { *argv.add(1) }
	} else {
		unsafe { crate::abi::pon_none() }
	};
	let key_os = std::ffi::OsStr::from_bytes(key_bytes);
	match std::env::var_os(key_os) {
		Some(value) => {
			let bytes = value.into_vec();
			unsafe { crate::abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
		},
		None => default,
	}
}
