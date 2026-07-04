//! Native `_posixsubprocess` seed backed by host `posix_spawn(3)`.
//!
//! CPython's pure-Python `subprocess` module delegates POSIX process creation
//! to one private entry point, `_posixsubprocess.fork_exec`.  Pon serves the
//! same ABI and uses `posix_spawn` rather than a raw `fork` so the child never
//! runs Rust or Python code in a forked multi-threaded runtime.

use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::ptr;

use num_traits::ToPrimitive;

use crate::abi::{pon_const_str, pon_make_function};
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::install_module;

#[cfg(target_vendor = "apple")]
const POSIX_SPAWN_SETSID_FLAG: libc::c_int = 0x0400;
#[cfg(not(target_vendor = "apple"))]
const POSIX_SPAWN_SETSID_FLAG: libc::c_int = libc::POSIX_SPAWN_SETSID as libc::c_int;

const DUMMY_ERROR_PID: libc::pid_t = i32::MAX as libc::pid_t;

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: *const libc::c_char,
    ) -> libc::c_int;
    fn posix_spawn_file_actions_addinherit_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
    ) -> libc::c_int;
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_posixsubprocess";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _posixsubprocess.__name__".to_owned());
    }
    let fork_exec_name = intern("fork_exec");
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let fork_exec = unsafe {
        pon_make_function(
            posixsubprocess_fork_exec as *const u8,
            crate::builtins::variadic_arity(),
            fork_exec_name,
        )
    };
    if fork_exec.is_null() {
        return Err("failed to allocate _posixsubprocess.fork_exec".to_owned());
    }
    install_module(
        name,
        vec![(intern("__name__"), name_obj), (fork_exec_name, fork_exec)],
    )
}

struct FileActions {
    inner: libc::posix_spawn_file_actions_t,
}

impl FileActions {
    fn new() -> Result<Self, libc::c_int> {
        let mut inner = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
        // SAFETY: `inner` is an out-slot for the libc initializer.
        let rc = unsafe { libc::posix_spawn_file_actions_init(inner.as_mut_ptr()) };
        if rc != 0 {
            return Err(rc);
        }
        // SAFETY: libc reported successful initialization.
        Ok(Self { inner: unsafe { inner.assume_init() } })
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        &self.inner
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawn_file_actions_t {
        &mut self.inner
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        // SAFETY: `inner` was initialized by `posix_spawn_file_actions_init`.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.inner) };
    }
}

struct SpawnAttr {
    inner: libc::posix_spawnattr_t,
}

impl SpawnAttr {
    fn new() -> Result<Self, libc::c_int> {
        let mut inner = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
        // SAFETY: `inner` is an out-slot for the libc initializer.
        let rc = unsafe { libc::posix_spawnattr_init(inner.as_mut_ptr()) };
        if rc != 0 {
            return Err(rc);
        }
        // SAFETY: libc reported successful initialization.
        Ok(Self { inner: unsafe { inner.assume_init() } })
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        &self.inner
    }

    fn as_mut_ptr(&mut self) -> *mut libc::posix_spawnattr_t {
        &mut self.inner
    }
}

impl Drop for SpawnAttr {
    fn drop(&mut self) {
        // SAFETY: `inner` was initialized by `posix_spawnattr_init`.
        unsafe { libc::posix_spawnattr_destroy(&mut self.inner) };
    }
}

struct CStringArray {
    _storage: Vec<CString>,
    ptrs: Vec<*mut libc::c_char>,
}

impl CStringArray {
    fn from_storage(storage: Vec<CString>) -> Self {
        let mut ptrs = storage
            .iter()
            .map(|value| value.as_ptr().cast_mut())
            .collect::<Vec<*mut libc::c_char>>();
        ptrs.push(ptr::null_mut());
        Self { _storage: storage, ptrs }
    }

    fn as_ptr(&self) -> *const *mut libc::c_char {
        self.ptrs.as_ptr()
    }
}

unsafe extern "C" fn posixsubprocess_fork_exec(
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let spec = match ForkExecSpec::parse(args) {
        Ok(spec) => spec,
        Err(error) => return error,
    };
    match spawn_child(&spec) {
        Ok(pid) => unsafe { crate::abi::pon_const_int(i64::from(pid)) },
        Err(SpawnError::Setup { errno, context }) => raise_setup_errno(errno, context),
        Err(SpawnError::Child { errno, message }) => {
            write_child_error(spec.errpipe_write, errno, message);
            // CPython normally returns a real child that wrote this protocol
            // before exiting.  `posix_spawn` reports pre-exec failures in the
            // parent, so return a never-child pid; subprocess.py catches the
            // resulting ChildProcessError from waitpid() and raises the parsed
            // FileNotFoundError/OSError from `errpipe_data`.
            unsafe { crate::abi::pon_const_int(i64::from(DUMMY_ERROR_PID)) }
        }
    }
}

struct ForkExecSpec {
    args: Option<CStringArray>,
    executable_list: Vec<CString>,
    close_fds: bool,
    fds_to_keep: Vec<libc::c_int>,
    cwd: Option<CString>,
    env: Option<CStringArray>,
    p2cread: libc::c_int,
    p2cwrite: libc::c_int,
    c2pread: libc::c_int,
    c2pwrite: libc::c_int,
    errread: libc::c_int,
    errwrite: libc::c_int,
    errpipe_read: libc::c_int,
    errpipe_write: libc::c_int,
    restore_signals: bool,
    call_setsid: bool,
    pgid_to_set: libc::pid_t,
}

impl ForkExecSpec {
    fn parse(args: &[*mut PyObject]) -> Result<Self, *mut PyObject> {
        if args.len() != 22 {
            return Err(crate::abi::return_null_with_error(format!(
                "_posixsubprocess.fork_exec expected 22 arguments, got {}",
                args.len()
            )));
        }

        let pgid_to_set = int_arg(args[16], "pgid_to_set")?;
        if pgid_to_set < -1 || pgid_to_set > i64::from(i32::MAX) {
            return Err(value_error("pgid_to_set must be -1 or a non-negative pid_t"));
        }
        let pgid_to_set = pgid_to_set as libc::pid_t;
        if !is_none(args[17]) {
            return Err(unsupported("_posixsubprocess.fork_exec does not implement gid changes"));
        }
        if !is_none(args[18]) {
            return Err(unsupported("_posixsubprocess.fork_exec does not implement extra_groups/setgroups"));
        }
        if !is_none(args[19]) {
            return Err(unsupported("_posixsubprocess.fork_exec does not implement uid changes"));
        }
        let child_umask = int_arg(args[20], "child_umask")?;
        if child_umask >= 0 {
            return Err(unsupported("_posixsubprocess.fork_exec does not implement child umask changes"));
        }
        if !is_none(args[21]) {
            return Err(unsupported("_posixsubprocess.fork_exec does not implement preexec_fn"));
        }

        let process_args = if is_none(args[0]) {
            None
        } else {
            let entries = sequence_items(args[0], "args")?;
            if entries.is_empty() {
                return Err(value_error("_posixsubprocess.fork_exec args must not be empty"));
            }
            let storage = entries
                .iter()
                .copied()
                .enumerate()
                .map(|(index, object)| object_to_cstring(object, &format!("args[{index}]")))
                .collect::<Result<Vec<_>, _>>()?;
            Some(CStringArray::from_storage(storage))
        };

        let exec_entries = sequence_items(args[1], "executable_list")?;
        if exec_entries.is_empty() {
            return Err(value_error("_posixsubprocess.fork_exec executable_list must not be empty"));
        }
        let executable_list = exec_entries
            .iter()
            .copied()
            .enumerate()
            .map(|(index, object)| object_to_cstring(object, &format!("executable_list[{index}]")))
            .collect::<Result<Vec<_>, _>>()?;

        let fds_to_keep_entries = sequence_items(args[3], "fds_to_keep")?;
        let mut fds_to_keep = Vec::with_capacity(fds_to_keep_entries.len());
        let mut previous = -1;
        for (index, object) in fds_to_keep_entries.iter().copied().enumerate() {
            let fd = int_arg(object, &format!("fds_to_keep[{index}]"))?;
            if fd < 0 || fd > i64::from(i32::MAX) || fd <= i64::from(previous) {
                return Err(value_error("bad value(s) in fds_to_keep"));
            }
            previous = fd as libc::c_int;
            fds_to_keep.push(previous);
        }

        let cwd = if is_none(args[4]) {
            None
        } else {
            Some(object_to_cstring(args[4], "cwd")?)
        };

        let env = if is_none(args[5]) {
            None
        } else {
            let env_entries = sequence_items(args[5], "env_list")?;
            let storage = env_entries
                .iter()
                .copied()
                .enumerate()
                .map(|(index, object)| object_to_cstring(object, &format!("env_list[{index}]")))
                .collect::<Result<Vec<_>, _>>()?;
            Some(CStringArray::from_storage(storage))
        };

        Ok(Self {
            args: process_args,
            executable_list,
            close_fds: bool_arg(args[2], "close_fds")?,
            fds_to_keep,
            cwd,
            env,
            p2cread: fd_arg(args[6], "p2cread")?,
            p2cwrite: fd_arg(args[7], "p2cwrite")?,
            c2pread: fd_arg(args[8], "c2pread")?,
            c2pwrite: fd_arg(args[9], "c2pwrite")?,
            errread: fd_arg(args[10], "errread")?,
            errwrite: fd_arg(args[11], "errwrite")?,
            errpipe_read: fd_arg(args[12], "errpipe_read")?,
            errpipe_write: fd_arg(args[13], "errpipe_write")?,
            restore_signals: bool_arg(args[14], "restore_signals")?,
            call_setsid: bool_arg(args[15], "call_setsid")?,
            pgid_to_set,
        })
    }
}

fn spawn_child(spec: &ForkExecSpec) -> Result<libc::pid_t, SpawnError> {
    let mut actions = FileActions::new().map_err(|errno| SpawnError::Setup {
        errno,
        context: "posix_spawn_file_actions_init",
    })?;
    let mut attr = SpawnAttr::new().map_err(|errno| SpawnError::Setup {
        errno,
        context: "posix_spawnattr_init",
    })?;

    if let Some(cwd) = &spec.cwd {
        if let Some(errno) = cwd_precheck_errno(cwd) {
            return Err(SpawnError::Child {
                errno,
                message: "noexec:chdir",
            });
        }
        add_chdir(&mut actions, cwd).map_err(|errno| SpawnError::Setup {
            errno,
            context: "posix_spawn_file_actions_addchdir_np",
        })?;
    }

    for fd in [spec.p2cwrite, spec.c2pread, spec.errread, spec.errpipe_read] {
        add_close(&mut actions, fd).map_err(|errno| SpawnError::Setup {
            errno,
            context: "posix_spawn_file_actions_addclose",
        })?;
    }
    for (source, target) in [(spec.p2cread, 0), (spec.c2pwrite, 1), (spec.errwrite, 2)] {
        if source != -1 {
            add_dup2(&mut actions, source, target).map_err(|errno| SpawnError::Setup {
                errno,
                context: "posix_spawn_file_actions_adddup2",
            })?;
            if source != target && source > 2 && !spec.fds_to_keep.contains(&source) {
                add_close(&mut actions, source).map_err(|errno| SpawnError::Setup {
                    errno,
                    context: "posix_spawn_file_actions_addclose",
                })?;
            }
        }
    }
    add_close(&mut actions, spec.errpipe_write).map_err(|errno| SpawnError::Setup {
        errno,
        context: "posix_spawn_file_actions_addclose",
    })?;

    let mut flags: libc::c_int = 0;
    if spec.restore_signals {
        install_default_signal_set(&mut attr)?;
        flags |= libc::POSIX_SPAWN_SETSIGDEF as libc::c_int;
    }
    if spec.call_setsid {
        flags |= POSIX_SPAWN_SETSID_FLAG;
    }
    if spec.pgid_to_set >= 0 {
        add_spawn_call(unsafe { libc::posix_spawnattr_setpgroup(attr.as_mut_ptr(), spec.pgid_to_set) }, "posix_spawnattr_setpgroup")?;
        flags |= libc::POSIX_SPAWN_SETPGROUP as libc::c_int;
    }
    configure_close_fds(&mut actions, spec, &mut flags)?;
    if flags != 0 {
        add_spawn_call(
            unsafe { libc::posix_spawnattr_setflags(attr.as_mut_ptr(), flags as libc::c_short) },
            "posix_spawnattr_setflags",
        )?;
    }

    let envp = spec
        .env
        .as_ref()
        .map_or_else(inherited_envp, CStringArray::as_ptr);

    let mut selected_errno = 0;
    let mut last_errno = libc::ENOENT;
    for executable in &spec.executable_list {
        let mut pid: libc::pid_t = 0;
        let fallback_argv = [executable.as_ptr().cast_mut(), ptr::null_mut()];
        let argv = spec
            .args
            .as_ref()
            .map_or(fallback_argv.as_ptr(), CStringArray::as_ptr);
        // SAFETY: All pointers reference C strings/NULL-terminated arrays kept
        // alive by `spec` and locals for the duration of the call.
        let rc = unsafe {
            libc::posix_spawn(
                &mut pid,
                executable.as_ptr(),
                actions.as_ptr(),
                attr.as_ptr(),
                argv,
                envp,
            )
        };
        if rc == 0 {
            return Ok(pid);
        }
        last_errno = rc;
        if rc != libc::ENOENT && rc != libc::ENOTDIR && selected_errno == 0 {
            selected_errno = rc;
        }
    }

    Err(SpawnError::Child {
        errno: if selected_errno != 0 { selected_errno } else { last_errno },
        message: "noexec",
    })
}

#[derive(Debug)]
enum SpawnError {
    Setup { errno: libc::c_int, context: &'static str },
    Child { errno: libc::c_int, message: &'static str },
}

fn install_default_signal_set(attr: &mut SpawnAttr) -> Result<(), SpawnError> {
    let mut sigset = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: `sigset` is an out-slot for the signal-set initializer.
    if unsafe { libc::sigemptyset(sigset.as_mut_ptr()) } != 0 {
        return Err(SpawnError::Setup {
            errno: last_errno(),
            context: "sigemptyset",
        });
    }
    // SAFETY: libc reported successful initialization.
    let mut sigset = unsafe { sigset.assume_init() };
    for signal in restore_signal_numbers() {
        // SAFETY: `sigset` is initialized; signal constants are host-provided.
        if unsafe { libc::sigaddset(&mut sigset, signal) } != 0 {
            return Err(SpawnError::Setup {
                errno: last_errno(),
                context: "sigaddset",
            });
        }
    }
    add_spawn_call(
        unsafe { libc::posix_spawnattr_setsigdefault(attr.as_mut_ptr(), &sigset) },
        "posix_spawnattr_setsigdefault",
    )
}

fn restore_signal_numbers() -> Vec<libc::c_int> {
    vec![libc::SIGPIPE, libc::SIGXFSZ]
}

#[cfg(target_vendor = "apple")]
fn configure_close_fds(
    actions: &mut FileActions,
    spec: &ForkExecSpec,
    flags: &mut libc::c_int,
) -> Result<(), SpawnError> {
    if !spec.close_fds {
        return Ok(());
    }
    *flags |= libc::POSIX_SPAWN_CLOEXEC_DEFAULT as libc::c_int;
    for (fd, redirected) in [
        (0, spec.p2cread != -1),
        (1, spec.c2pwrite != -1),
        (2, spec.errwrite != -1),
    ] {
        if !redirected {
            add_inherit(actions, fd)?;
        }
    }
    for fd in spec.fds_to_keep.iter().copied() {
        if fd != spec.errpipe_write {
            add_inherit(actions, fd)?;
        }
    }
    Ok(())
}

#[cfg(not(target_vendor = "apple"))]
fn configure_close_fds(
    _actions: &mut FileActions,
    spec: &ForkExecSpec,
    _flags: &mut libc::c_int,
) -> Result<(), SpawnError> {
    if spec.close_fds && spec.fds_to_keep.iter().any(|fd| *fd != spec.errpipe_write) {
        return Err(SpawnError::Setup {
            errno: libc::ENOSYS,
            context: "close_fds with pass_fds",
        });
    }
    Ok(())
}

fn add_spawn_call(rc: libc::c_int, context: &'static str) -> Result<(), SpawnError> {
    if rc == 0 {
        Ok(())
    } else {
        Err(SpawnError::Setup { errno: rc, context })
    }
}

fn add_close(actions: &mut FileActions, fd: libc::c_int) -> Result<(), libc::c_int> {
    if fd < 0 {
        return Ok(());
    }
    // SAFETY: `actions` is initialized; libc validates `fd`.
    let rc = unsafe { libc::posix_spawn_file_actions_addclose(actions.as_mut_ptr(), fd) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

fn add_dup2(actions: &mut FileActions, fd: libc::c_int, target: libc::c_int) -> Result<(), libc::c_int> {
    // SAFETY: `actions` is initialized; libc validates descriptors.
    let rc = unsafe { libc::posix_spawn_file_actions_adddup2(actions.as_mut_ptr(), fd, target) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

#[cfg(target_vendor = "apple")]
fn add_inherit(actions: &mut FileActions, fd: libc::c_int) -> Result<(), SpawnError> {
    // SAFETY: `actions` is initialized; libc validates descriptors.
    let rc = unsafe { posix_spawn_file_actions_addinherit_np(actions.as_mut_ptr(), fd) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SpawnError::Setup {
            errno: rc,
            context: "posix_spawn_file_actions_addinherit_np",
        })
    }
}

#[cfg(target_vendor = "apple")]
fn add_chdir(actions: &mut FileActions, cwd: &CString) -> Result<(), libc::c_int> {
    // SAFETY: `actions` is initialized and `cwd` is a live C string.
    let rc = unsafe { posix_spawn_file_actions_addchdir_np(actions.as_mut_ptr(), cwd.as_ptr()) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

#[cfg(not(target_vendor = "apple"))]
fn add_chdir(actions: &mut FileActions, cwd: &CString) -> Result<(), libc::c_int> {
    // SAFETY: `actions` is initialized and `cwd` is a live C string.
    let rc = unsafe { libc::posix_spawn_file_actions_addchdir_np(actions.as_mut_ptr(), cwd.as_ptr()) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

fn cwd_precheck_errno(cwd: &CString) -> Option<libc::c_int> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is an out-slot and `cwd` is a live C string.
    if unsafe { libc::stat(cwd.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Some(last_errno());
    }
    // SAFETY: `stat` succeeded.
    let stat = unsafe { stat.assume_init() };
    if (stat.st_mode & libc::S_IFMT) != libc::S_IFDIR {
        return Some(libc::ENOTDIR);
    }
    // SAFETY: `cwd` is a live C string; libc performs permission checks.
    if unsafe { libc::access(cwd.as_ptr(), libc::X_OK) } != 0 {
        return Some(last_errno());
    }
    None
}

fn object_to_cstring(object: *mut PyObject, what: &str) -> Result<CString, *mut PyObject> {
    let bytes = object_to_bytes(object, what)?;
    CString::new(bytes).map_err(|_| value_error(&format!("{what} must not contain embedded null byte")))
}

fn object_to_bytes(object: *mut PyObject, _what: &str) -> Result<Vec<u8>, *mut PyObject> {
    let coerced = fspath_coerce(object)?;
    let raw = crate::tag::untag_arg(coerced);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(path_result_type_error(object, raw));
    }
    // SAFETY: Heap pointer with a live header after the checks above.
    if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
        return Ok(text.as_bytes().to_vec());
    }
    // SAFETY: Heap pointer with a live header after the checks above.
    if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
        // SAFETY: Type check proved the PyBytes layout.
        return Ok(unsafe { (*raw.cast::<crate::types::bytes_::PyBytes>()).as_slice() }.to_vec());
    }
    Err(path_result_type_error(object, raw))
}

fn fspath_coerce(object: *mut PyObject) -> Result<*mut PyObject, *mut PyObject> {
    let raw = crate::tag::untag_arg(object);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if matches!(unsafe { crate::types::dict::type_name(raw) }, Some("str" | "bytes")) {
            return Ok(object);
        }
        // SAFETY: Heap pointer with a live header after the tag checks.
        let ty = unsafe { (*raw).ob_type.cast_mut() };
        let hook = unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) };
        if !hook.is_null() {
            let bound = unsafe { crate::descr::descriptor_get(hook, raw, ty) };
            if bound.is_null() {
                return Err(ptr::null_mut());
            }
            let result = unsafe { crate::abi::pon_call(bound, ptr::null_mut(), 0) };
            if result.is_null() {
                return Err(ptr::null_mut());
            }
            return Ok(result);
        }
    }
    Err(type_error(&format!(
        "expected str, bytes or os.PathLike object, not {}",
        object_type_display(raw)
    )))
}

fn path_result_type_error(source: *mut PyObject, result: *mut PyObject) -> *mut PyObject {
    let source_raw = crate::tag::untag_arg(source);
    type_error(&format!(
        "expected {}.__fspath__() to return str or bytes, not {}",
        object_type_display(source_raw),
        object_type_display(result)
    ))
}

fn object_type_display(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        "NoneType"
    } else if crate::tag::is_small_int(object) {
        "int"
    } else {
        // SAFETY: Heap pointer with a live header after the tag checks above.
        unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
    }
}

fn sequence_items<'a>(object: *mut PyObject, what: &str) -> Result<&'a [*mut PyObject], *mut PyObject> {
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(type_error(&format!("{what} must be a list or tuple")));
    }
    // SAFETY: Heap pointer with a live header after the checks above.
    match unsafe { crate::types::dict::type_name(raw) } {
        Some("list") => Ok(unsafe { (*raw.cast::<crate::types::list::PyList>()).as_slice() }),
        Some("tuple") => Ok(unsafe { (*raw.cast::<crate::types::tuple::PyTuple>()).as_slice() }),
        _ => Err(type_error(&format!("{what} must be a list or tuple"))),
    }
}

fn fd_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
    let value = int_arg(object, what)?;
    if value < i64::from(libc::c_int::MIN) || value > i64::from(libc::c_int::MAX) {
        return Err(value_error(&format!("{what} is out of range")));
    }
    Ok(value as libc::c_int)
}

fn bool_arg(object: *mut PyObject, what: &str) -> Result<bool, *mut PyObject> {
    int_arg(object, what).map(|value| value != 0)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    if crate::tag::is_small_int(object) {
        return Ok(crate::tag::untag_small_int(object));
    }
    // SAFETY: Non-immediate pointers are boxed objects; conversion type-checks.
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => value.to_i64().ok_or_else(|| value_error(&format!("{what} is too large to fit in a C integer"))),
        None => Err(type_error(&format!("{what} must be an integer"))),
    }
}

fn is_none(object: *mut PyObject) -> bool {
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

unsafe fn call_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argv.is_null() || argc == 0 {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

fn unsupported(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::NotImplementedError, message)
}

fn type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn value_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn raise_setup_errno(errno: libc::c_int, context: &'static str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(
        ExceptionKind::OSError,
        &format!("{context} failed: [Errno {errno}] {}", errno_text(errno)),
    )
}

fn write_child_error(fd: libc::c_int, errno: libc::c_int, message: &str) {
    let payload = if errno != 0 {
        format!("{}:{errno:x}:{message}", exception_name_for_errno(errno))
    } else {
        format!("SubprocessError:0:{message}")
    };
    write_all_no_raise(fd, payload.as_bytes());
}

fn exception_name_for_errno(errno: libc::c_int) -> &'static str {
    match errno {
        libc::ENOENT => "FileNotFoundError",
        libc::ENOTDIR => "NotADirectoryError",
        libc::EACCES | libc::EPERM => "PermissionError",
        libc::EINTR => "InterruptedError",
        libc::EPIPE => "BrokenPipeError",
        libc::ECHILD => "ChildProcessError",
        libc::ESRCH => "ProcessLookupError",
        libc::EAGAIN => "BlockingIOError",
        libc::ETIMEDOUT => "TimeoutError",
        libc::ECONNABORTED => "ConnectionAbortedError",
        libc::ECONNREFUSED => "ConnectionRefusedError",
        libc::ECONNRESET => "ConnectionResetError",
        _ => "OSError",
    }
}

fn write_all_no_raise(fd: libc::c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a readable memory window for libc to consume.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
            continue;
        }
        if written < 0 && last_errno() == libc::EINTR {
            continue;
        }
        break;
    }
}

fn errno_text(errno: libc::c_int) -> String {
    // SAFETY: `strerror` returns a NUL-terminated static message for `errno`.
    unsafe { CStr::from_ptr(libc::strerror(errno)) }
        .to_string_lossy()
        .into_owned()
}

fn last_errno() -> libc::c_int {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

#[cfg(target_vendor = "apple")]
fn inherited_envp() -> *const *mut libc::c_char {
    // SAFETY: `_NSGetEnviron` returns the process global `environ` slot.
    unsafe { *libc::_NSGetEnviron() }.cast_const()
}

#[cfg(not(target_vendor = "apple"))]
fn inherited_envp() -> *const *mut libc::c_char {
    unsafe extern "C" {
        static mut environ: *mut *mut libc::c_char;
    }
    unsafe { environ.cast_const() }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;
    use crate::abi;
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};
    use crate::types::type_::{build_class_from_namespace, new_namespace, type_new};

    fn init_runtime() {
        assert_eq!(unsafe { abi::pon_runtime_init() }, 0);
        pon_err_clear();
    }

    unsafe extern "C" fn fspath_returns_tmp(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        let path = b"/tmp";
        unsafe { abi::pon_const_str(path.as_ptr(), path.len()) }
    }

    unsafe fn pathlike_instance() -> *mut PyObject {
        let fspath_name = intern("__fspath__");
        let fspath = unsafe { abi::pon_make_function(fspath_returns_tmp as *const u8, 1, fspath_name) };
        assert!(!fspath.is_null(), "failed to allocate __fspath__ function");
        let namespace = new_namespace();
        unsafe {
            (&mut *namespace).set(fspath_name, fspath);
        }
        let class = unsafe { build_class_from_namespace("Pathish", &[], namespace, &[]) }.cast::<crate::object::PyType>();
        assert!(!class.is_null(), "failed to build Pathish class: {:?}", pon_err_message());
        let instance = unsafe { type_new(class, ptr::null_mut(), ptr::null_mut()) };
        assert!(!instance.is_null(), "failed to allocate Pathish instance: {:?}", pon_err_message());
        instance
    }

    #[test]
    fn object_to_bytes_accepts_pathlike_fspath_result() {
        let _guard = test_state_lock();
        init_runtime();
        let instance = unsafe { pathlike_instance() };

        let bytes = object_to_bytes(instance, "cwd").expect("PathLike __fspath__ should coerce to bytes");

        assert_eq!(bytes, b"/tmp");
    }
}
