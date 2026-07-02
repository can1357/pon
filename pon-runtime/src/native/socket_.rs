//! Native `_socket` seed: import surface only.
//!
//! The CT frontier needs `import socket` to succeed — `unittest.mock` pulls
//! `asyncio` → `asyncio.base_events` → `socket`, and `socketserver` imports
//! it directly — but nothing on that frontier needs live sockets at import
//! time.  This module gives the vendored `Lib/socket.py` exactly what its
//! module body consumes: the address/type/flag constants (real host values
//! via `libc`, feeding `IntEnum._convert_` for `AddressFamily`/`SocketKind`
//! and `IntFlag._convert_` for `MsgFlag`/`AddressInfo`), a subclassable
//! `socket` type whose constructor raises `OSError` honestly on use, the
//! `gaierror`/`herror` exception classes (OSError subclasses, the binascii
//! heap-class recipe), `error`/`timeout` as their CPython aliases (`OSError`
//! / `TimeoutError`), `has_ipv6`, and the resolver/byte-order functions as
//! honest `OSError` raisers — loud at the exact call site instead of a
//! silent wrong result.
//!
//! Deliberate absences keep stdlib `hasattr` probes on their clean fallback
//! paths: no `socketpair` (socket.py falls back to `_fallback_socketpair`),
//! no `sendmsg`/`recvmsg`/`share` on the socket type (no
//! `send_fds`/`recv_fds`/`fromshare`), and no `inet_pton`/`inet_ntop`
//! (asyncio's `_ipaddr_info` short-circuits to None).
//! `getdefaulttimeout`/`setdefaulttimeout` are real: they are pure module
//! state that `test.support.socket_helper` drives in try/finally blocks.

use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_set};
use crate::types::type_::{self as type_mod};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Address-family, socket-kind, message-flag, address-info, protocol, and
/// socket-option constants the vendored stdlib reads, all sourced from the
/// host libc so the values are honest for this platform.  socket.py's
/// `IntEnum._convert_`/`IntFlag._convert_` sweeps pick these up by prefix.
const INT_CONSTANTS: &[(&str, i64)] = &[
    ("AF_INET", libc::AF_INET as i64),
    ("AF_INET6", libc::AF_INET6 as i64),
    ("AF_UNIX", libc::AF_UNIX as i64),
    ("AF_UNSPEC", libc::AF_UNSPEC as i64),
    ("AI_ADDRCONFIG", libc::AI_ADDRCONFIG as i64),
    ("AI_ALL", libc::AI_ALL as i64),
    ("AI_CANONNAME", libc::AI_CANONNAME as i64),
    ("AI_NUMERICHOST", libc::AI_NUMERICHOST as i64),
    ("AI_NUMERICSERV", libc::AI_NUMERICSERV as i64),
    ("AI_PASSIVE", libc::AI_PASSIVE as i64),
    ("AI_V4MAPPED", libc::AI_V4MAPPED as i64),
    ("INADDR_ANY", libc::INADDR_ANY as i64),
    ("INADDR_BROADCAST", libc::INADDR_BROADCAST as i64),
    ("INADDR_LOOPBACK", libc::INADDR_LOOPBACK as i64),
    ("INADDR_NONE", libc::INADDR_NONE as i64),
    ("IPPROTO_ICMP", libc::IPPROTO_ICMP as i64),
    ("IPPROTO_IP", libc::IPPROTO_IP as i64),
    ("IPPROTO_IPV6", libc::IPPROTO_IPV6 as i64),
    ("IPPROTO_RAW", libc::IPPROTO_RAW as i64),
    ("IPPROTO_TCP", libc::IPPROTO_TCP as i64),
    ("IPPROTO_UDP", libc::IPPROTO_UDP as i64),
    ("IPV6_V6ONLY", libc::IPV6_V6ONLY as i64),
    ("MSG_CTRUNC", libc::MSG_CTRUNC as i64),
    ("MSG_DONTROUTE", libc::MSG_DONTROUTE as i64),
    ("MSG_DONTWAIT", libc::MSG_DONTWAIT as i64),
    ("MSG_EOR", libc::MSG_EOR as i64),
    ("MSG_OOB", libc::MSG_OOB as i64),
    ("MSG_PEEK", libc::MSG_PEEK as i64),
    ("MSG_TRUNC", libc::MSG_TRUNC as i64),
    ("MSG_WAITALL", libc::MSG_WAITALL as i64),
    ("SHUT_RD", libc::SHUT_RD as i64),
    ("SHUT_RDWR", libc::SHUT_RDWR as i64),
    ("SHUT_WR", libc::SHUT_WR as i64),
    ("SOCK_DGRAM", libc::SOCK_DGRAM as i64),
    ("SOCK_RAW", libc::SOCK_RAW as i64),
    ("SOCK_RDM", libc::SOCK_RDM as i64),
    ("SOCK_SEQPACKET", libc::SOCK_SEQPACKET as i64),
    ("SOCK_STREAM", libc::SOCK_STREAM as i64),
    ("SOL_SOCKET", libc::SOL_SOCKET as i64),
    ("SOMAXCONN", libc::SOMAXCONN as i64),
    ("SO_ACCEPTCONN", libc::SO_ACCEPTCONN as i64),
    ("SO_BROADCAST", libc::SO_BROADCAST as i64),
    ("SO_DEBUG", libc::SO_DEBUG as i64),
    ("SO_DONTROUTE", libc::SO_DONTROUTE as i64),
    ("SO_ERROR", libc::SO_ERROR as i64),
    ("SO_KEEPALIVE", libc::SO_KEEPALIVE as i64),
    ("SO_LINGER", libc::SO_LINGER as i64),
    ("SO_OOBINLINE", libc::SO_OOBINLINE as i64),
    ("SO_RCVBUF", libc::SO_RCVBUF as i64),
    ("SO_RCVLOWAT", libc::SO_RCVLOWAT as i64),
    ("SO_RCVTIMEO", libc::SO_RCVTIMEO as i64),
    ("SO_REUSEADDR", libc::SO_REUSEADDR as i64),
    ("SO_REUSEPORT", libc::SO_REUSEPORT as i64),
    ("SO_SNDBUF", libc::SO_SNDBUF as i64),
    ("SO_SNDLOWAT", libc::SO_SNDLOWAT as i64),
    ("SO_SNDTIMEO", libc::SO_SNDTIMEO as i64),
    ("SO_TYPE", libc::SO_TYPE as i64),
    #[cfg(target_os = "macos")]
    ("TCP_KEEPALIVE", libc::TCP_KEEPALIVE as i64),
    ("TCP_KEEPCNT", libc::TCP_KEEPCNT as i64),
    #[cfg(target_os = "linux")]
    ("TCP_KEEPIDLE", libc::TCP_KEEPIDLE as i64),
    ("TCP_KEEPINTVL", libc::TCP_KEEPINTVL as i64),
    ("TCP_MAXSEG", libc::TCP_MAXSEG as i64),
    ("TCP_NODELAY", libc::TCP_NODELAY as i64),
];

/// Resolver and byte-order entry points that need a live network stack;
/// each raises `OSError` honestly when called.  `socketpair`, `inet_pton`,
/// and `inet_ntop` are deliberately NOT here (see the module docs).
const NOT_WIRED_FUNCTIONS: &[(&str, BuiltinFn)] = &[
    ("close", socket_close),
    ("dup", socket_dup),
    ("getaddrinfo", socket_getaddrinfo),
    ("gethostbyaddr", socket_gethostbyaddr),
    ("gethostbyname", socket_gethostbyname),
    ("gethostbyname_ex", socket_gethostbyname_ex),
    ("gethostname", socket_gethostname),
    ("getnameinfo", socket_getnameinfo),
    ("getprotobyname", socket_getprotobyname),
    ("getservbyname", socket_getservbyname),
    ("getservbyport", socket_getservbyport),
    ("htonl", socket_htonl),
    ("htons", socket_htons),
    ("inet_aton", socket_inet_aton),
    ("inet_ntoa", socket_inet_ntoa),
    ("ntohl", socket_ntohl),
    ("ntohs", socket_ntohs),
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_socket";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _socket.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_obj)];
    for &(const_name, value) in INT_CONSTANTS {
        // SAFETY: Runtime allocation helper; NULL is checked below.
        let object = unsafe { abi::pon_const_int(value) };
        if object.is_null() {
            return Err(format!("failed to allocate _socket.{const_name}"));
        }
        attrs.push((intern(const_name), object));
    }
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let has_ipv6 = unsafe { abi::pon_const_bool(1) };
    if has_ipv6.is_null() {
        return Err("failed to allocate _socket.has_ipv6".to_owned());
    }
    attrs.push((intern("has_ipv6"), has_ipv6));

    // `socket.error`/`socket.timeout` have been aliases of the builtins
    // since 3.3/3.10; loading the registered classes keeps
    // `socket.error is OSError` and `socket.timeout is TimeoutError` true.
    attrs.push((intern("error"), builtin_class("OSError")?));
    attrs.push((intern("timeout"), builtin_class("TimeoutError")?));

    let gaierror = *GAIERROR_CLASS;
    if gaierror == 0 {
        return Err("failed to create _socket.gaierror".to_owned());
    }
    attrs.push((intern("gaierror"), gaierror as *mut PyObject));
    let herror = *HERROR_CLASS;
    if herror == 0 {
        return Err("failed to create _socket.herror".to_owned());
    }
    attrs.push((intern("herror"), herror as *mut PyObject));

    let socket_type = socket_type().cast::<PyObject>();
    attrs.push((intern("socket"), socket_type));
    // CPython's C module exports the type under both names.
    attrs.push((intern("SocketType"), socket_type));

    for &(function_name, entry) in NOT_WIRED_FUNCTIONS {
        attrs.push(function_attr(function_name, entry)?);
    }
    attrs.push(function_attr("getdefaulttimeout", socket_getdefaulttimeout)?);
    attrs.push(function_attr("setdefaulttimeout", socket_setdefaulttimeout)?);

    install_module(name, attrs)
}

// ---------------------------------------------------------------------------
// Exception classes (the binascii heap-class recipe)

static GAIERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
    exception_class("gaierror", "OSError").map_or(0, |class| class as usize)
});

static HERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
    exception_class("herror", "OSError").map_or(0, |class| class as usize)
});

/// Builds one `socket` exception heap class deriving from the named builtin,
/// with `__module__` set — CPython names these `socket.gaierror`/`socket.herror`.
fn exception_class(name: &str, base: &str) -> Result<*mut PyObject, String> {
    let base_class = builtin_class(base)?;
    let namespace = type_mod::new_namespace();
    if namespace.is_null() {
        return Err(format!("failed to allocate _socket.{name} namespace"));
    }
    let module_object = alloc_str_object("socket");
    if module_object.is_null() {
        return Err(format!("failed to allocate _socket.{name}.__module__"));
    }
    // SAFETY: `new_namespace` returned a live namespace box.
    unsafe { (*namespace).set(intern("__module__"), module_object) };
    // SAFETY: The base is a live class object owned by the runtime.
    let class = unsafe { type_mod::build_class_from_namespace(name, &[base_class], namespace, &[]) };
    if class.is_null() {
        let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        pon_err_clear();
        return Err(format!("failed to create _socket.{name}: {detail}"));
    }
    // SAFETY: Freshly built class object; mirror `pon_build_class`'s ob_type fix.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = abi::runtime_type_type().cast_const();
        }
    }
    Ok(class)
}

fn builtin_class(name: &str) -> Result<*mut PyObject, String> {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let class = unsafe { abi::pon_load_global(intern(name), ptr::null_mut()) };
    if class.is_null() {
        pon_err_clear();
        return Err(format!("builtin class '{name}' is not registered"));
    }
    Ok(class)
}

// ---------------------------------------------------------------------------
// The socket type
//
// Subclassable (socket.py's `class socket(_socket.socket)` and the class
// statement machinery only need the type object and its MRO), but never
// instantiable: both the direct `tp_new` path and the `__init__` chain that
// heap subclasses resolve through the MRO raise `OSError` honestly.

/// Instances are never created; the layout exists only to size the type.
#[repr(C)]
struct PySocket {
    ob_base: PyObjectHeader,
}

static SOCKET_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "socket",
        std::mem::size_of::<PySocket>(),
    );
    ty.tp_base = runtime_object_type();
    ty.tp_new = Some(socket_new);
    // `class socket(_socket.socket)` in socket.py defines `__init__` calling
    // `_socket.socket.__init__(self, ...)`: the unbound raiser must resolve
    // through the type namespace.
    let namespace = type_mod::new_namespace();
    if !namespace.is_null() {
        // SAFETY: `entry` is a live builtin entry point with the runtime
        // calling convention.
        let init = unsafe {
            abi::pon_make_function(socket_init_method as *const u8, VARIADIC_ARITY, intern("__init__"))
        };
        if !init.is_null() {
            // SAFETY: `new_namespace` returned a live namespace box.
            unsafe { (*namespace).set(intern("__init__"), init) };
        }
        ty.tp_dict = namespace.cast::<PyObject>();
    }
    let ty = Box::into_raw(Box::new(ty));
    // GC rooting for the namespace's function object.
    crate::sync::register_namespaced_type(ty);
    ty as usize
});

fn socket_type() -> *mut PyType {
    *SOCKET_TYPE as *mut PyType
}

fn runtime_object_type() -> *mut PyType {
    abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

const SOCKET_NOT_WIRED: &str =
    "socket.socket is not wired to the host yet in the pon runtime (no real sockets; see native/socket_.rs)";

unsafe extern "C" fn socket_new(_cls: *mut PyType, _args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    // SAFETY: Typed raise helper; the message bytes are copied.
    unsafe { abi::exc::pon_raise_os_error(SOCKET_NOT_WIRED.as_ptr(), SOCKET_NOT_WIRED.len()) }
}

unsafe extern "C" fn socket_init_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // SAFETY: Typed raise helper; the message bytes are copied.
    unsafe { abi::exc::pon_raise_os_error(SOCKET_NOT_WIRED.as_ptr(), SOCKET_NOT_WIRED.len()) }
}

// ---------------------------------------------------------------------------
// Default-timeout state (real, not a stub: pure module state)

static DEFAULT_TIMEOUT: Mutex<Option<f64>> = Mutex::new(None);

unsafe extern "C" fn socket_getdefaulttimeout(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    let timeout = *DEFAULT_TIMEOUT.lock().unwrap_or_else(|poison| poison.into_inner());
    match timeout {
        // SAFETY: Runtime allocation helper; NULL propagates with the error set.
        Some(seconds) => unsafe { abi::number::pon_const_float(seconds) },
        // SAFETY: Singleton accessor.
        None => unsafe { abi::pon_none() },
    }
}

unsafe extern "C" fn socket_setdefaulttimeout(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("setdefaulttimeout received a NULL argv pointer");
    };
    if args.len() != 1 {
        let message = format!("setdefaulttimeout() takes exactly one argument ({} given)", args.len());
        return raise_type_error(&message);
    }
    let value = untag(args[0]);
    let timeout = if value == unsafe { abi::pon_none() } {
        None
    } else if let Some(seconds) = unsafe { crate::types::float::to_f64(value) } {
        Some(seconds)
    } else if let Some(seconds) = unsafe { crate::types::int::to_bigint(value) }.and_then(|big| num_bigint_to_f64(&big)) {
        Some(seconds)
    } else {
        return raise_type_error("a float is required");
    };
    if let Some(seconds) = timeout {
        if seconds < 0.0 {
            let message = "Timeout value out of range";
            // SAFETY: Typed raise helper; the message bytes are copied.
            return unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
        }
    }
    *DEFAULT_TIMEOUT.lock().unwrap_or_else(|poison| poison.into_inner()) = timeout;
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn num_bigint_to_f64(value: &num_bigint::BigInt) -> Option<f64> {
    use num_traits::ToPrimitive;
    value.to_f64()
}

// ---------------------------------------------------------------------------
// Honest not-wired raisers

macro_rules! not_wired {
    ($fn_name:ident, $py_name:literal) => {
        unsafe extern "C" fn $fn_name(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
            raise_not_wired(concat!("socket.", $py_name))
        }
    };
}

not_wired!(socket_close, "close");
not_wired!(socket_dup, "dup");
not_wired!(socket_getaddrinfo, "getaddrinfo");
not_wired!(socket_gethostbyaddr, "gethostbyaddr");
not_wired!(socket_gethostbyname, "gethostbyname");
not_wired!(socket_gethostbyname_ex, "gethostbyname_ex");
not_wired!(socket_gethostname, "gethostname");
not_wired!(socket_getnameinfo, "getnameinfo");
not_wired!(socket_getprotobyname, "getprotobyname");
not_wired!(socket_getservbyname, "getservbyname");
not_wired!(socket_getservbyport, "getservbyport");
not_wired!(socket_htonl, "htonl");
not_wired!(socket_htons, "htons");
not_wired!(socket_inet_aton, "inet_aton");
not_wired!(socket_inet_ntoa, "inet_ntoa");
not_wired!(socket_ntohl, "ntohl");
not_wired!(socket_ntohs, "ntohs");

/// Honest failure for the not-yet-wired network entry points.  OSError (the
/// `select` shim's convention) keeps stdlib `except OSError`/`except error`
/// paths meaningful.
fn raise_not_wired(which: &str) -> *mut PyObject {
    let message = format!("{which} is not wired to the host yet in the pon runtime");
    // SAFETY: Typed raise helper; the message bytes are copied.
    unsafe { abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) }
}

// ---------------------------------------------------------------------------
// Helpers (contextvars idioms)

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let object = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _socket.{name}"))
}
