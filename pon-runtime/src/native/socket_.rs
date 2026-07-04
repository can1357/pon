//! Native `_socket` seed: import surface plus host byte-order/address helpers.
//!
//! The CT frontier needs `import socket` to succeed — `unittest.mock` pulls
//! `asyncio` → `asyncio.base_events` → `socket`, and `socketserver` imports
//! it directly.  This module gives the vendored `Lib/socket.py` the address,
//! type, flag, protocol, resolver-error, and socket-option constants CPython's
//! Darwin build exposes, plus the pure C helper functions that do not require
//! owning live socket objects (`inet_pton`/`inet_ntop`, interface-name lookup,
//! ancillary-data sizing, default-timeout state, and `sethostname`).
//!
//! Constants use `libc` where the crate exposes the Darwin header value; a few
//! CPython-visible Darwin constants missing from `libc` are spelled as their
//! platform header values below.  `socket.py`'s `IntEnum._convert_` /
//! `IntFlag._convert_` sweeps pick them up by prefix.
//!
//! The `socket` heap type is still only subclassable, not a live fd wrapper:
//! constructing it raises `OSError`.  Deliberate absences keep stdlib probes
//! on clean fallback paths: no `_socket.socketpair` (socket.py falls back to
//! `_fallback_socketpair`) and no `sendmsg`/`recvmsg`/`share` methods on the
//! socket type (no `send_fds`/`recv_fds`/`fromshare`).  Resolver entry points
//! that need live socket objects still raise `OSError` loudly at the call site.

use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_set};
use crate::types::exc::ExceptionKind;
use crate::types::type_::{self as type_mod};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
unsafe extern "C" {
    fn inet_ntop(
        af: libc::c_int,
        src: *const libc::c_void,
        dst: *mut libc::c_char,
        size: libc::socklen_t,
    ) -> *const libc::c_char;
    fn inet_pton(af: libc::c_int, src: *const libc::c_char, dst: *mut libc::c_void) -> libc::c_int;
}

const INET6_ADDRSTRLEN: usize = 46;


/// Address-family, socket-kind, message-flag, address-info, protocol, and
/// socket-option constants CPython's Darwin `_socket` exposes.
const INT_CONSTANTS: &[(&str, i64)] = &[
    ("AF_APPLETALK", libc::AF_APPLETALK as i64),
    ("AF_DECnet", libc::AF_DECnet as i64),
    ("AF_INET", libc::AF_INET as i64),
    ("AF_INET6", libc::AF_INET6 as i64),
    ("AF_IPX", libc::AF_IPX as i64),
    ("AF_LINK", libc::AF_LINK as i64),
    ("AF_ROUTE", libc::AF_ROUTE as i64),
    ("AF_SNA", libc::AF_SNA as i64),
    ("AF_SYSTEM", libc::AF_SYSTEM as i64),
    ("AF_UNIX", libc::AF_UNIX as i64),
    ("AF_UNSPEC", libc::AF_UNSPEC as i64),
    ("AI_ADDRCONFIG", libc::AI_ADDRCONFIG as i64),
    ("AI_ALL", libc::AI_ALL as i64),
    ("AI_CANONNAME", libc::AI_CANONNAME as i64),
    ("AI_DEFAULT", libc::AI_DEFAULT as i64),
    ("AI_MASK", libc::AI_MASK as i64),
    ("AI_NUMERICHOST", libc::AI_NUMERICHOST as i64),
    ("AI_NUMERICSERV", libc::AI_NUMERICSERV as i64),
    ("AI_PASSIVE", libc::AI_PASSIVE as i64),
    ("AI_V4MAPPED", libc::AI_V4MAPPED as i64),
    ("AI_V4MAPPED_CFG", libc::AI_V4MAPPED_CFG as i64),
    // Darwin getaddrinfo(3) values exposed by CPython but not all by libc.
    ("EAI_ADDRFAMILY", 1),
    ("EAI_AGAIN", libc::EAI_AGAIN as i64),
    ("EAI_BADFLAGS", libc::EAI_BADFLAGS as i64),
    ("EAI_BADHINTS", 12),
    ("EAI_FAIL", libc::EAI_FAIL as i64),
    ("EAI_FAMILY", libc::EAI_FAMILY as i64),
    ("EAI_MAX", 15),
    ("EAI_MEMORY", libc::EAI_MEMORY as i64),
    ("EAI_NODATA", libc::EAI_NODATA as i64),
    ("EAI_NONAME", libc::EAI_NONAME as i64),
    ("EAI_OVERFLOW", libc::EAI_OVERFLOW as i64),
    ("EAI_PROTOCOL", 13),
    ("EAI_SERVICE", libc::EAI_SERVICE as i64),
    ("EAI_SOCKTYPE", libc::EAI_SOCKTYPE as i64),
    ("EAI_SYSTEM", libc::EAI_SYSTEM as i64),
    // net/ethernet.h Darwin header values.
    ("ETHERTYPE_ARP", 0x0806),
    ("ETHERTYPE_IP", 0x0800),
    ("ETHERTYPE_IPV6", 0x86DD),
    ("ETHERTYPE_VLAN", 0x8100),
    ("INADDR_ALLHOSTS_GROUP", 0xE0000001),
    ("INADDR_ANY", libc::INADDR_ANY as i64),
    ("INADDR_BROADCAST", libc::INADDR_BROADCAST as i64),
    ("INADDR_LOOPBACK", libc::INADDR_LOOPBACK as i64),
    ("INADDR_MAX_LOCAL_GROUP", 0xE00000FF),
    ("INADDR_NONE", libc::INADDR_NONE as i64),
    ("INADDR_UNSPEC_GROUP", 0xE0000000),
    ("IPPORT_RESERVED", 1024),
    ("IPPORT_USERRESERVED", 5000),
    ("IPPROTO_AH", libc::IPPROTO_AH as i64),
    ("IPPROTO_DSTOPTS", libc::IPPROTO_DSTOPTS as i64),
    ("IPPROTO_EGP", libc::IPPROTO_EGP as i64),
    ("IPPROTO_EON", libc::IPPROTO_EON as i64),
    ("IPPROTO_ESP", libc::IPPROTO_ESP as i64),
    ("IPPROTO_FRAGMENT", libc::IPPROTO_FRAGMENT as i64),
    ("IPPROTO_GGP", libc::IPPROTO_GGP as i64),
    ("IPPROTO_GRE", libc::IPPROTO_GRE as i64),
    ("IPPROTO_HELLO", libc::IPPROTO_HELLO as i64),
    ("IPPROTO_HOPOPTS", libc::IPPROTO_HOPOPTS as i64),
    ("IPPROTO_ICMP", libc::IPPROTO_ICMP as i64),
    ("IPPROTO_ICMPV6", libc::IPPROTO_ICMPV6 as i64),
    ("IPPROTO_IDP", libc::IPPROTO_IDP as i64),
    ("IPPROTO_IGMP", libc::IPPROTO_IGMP as i64),
    ("IPPROTO_IP", libc::IPPROTO_IP as i64),
    ("IPPROTO_IPCOMP", libc::IPPROTO_IPCOMP as i64),
    ("IPPROTO_IPIP", libc::IPPROTO_IPIP as i64),
    ("IPPROTO_IPV4", libc::IPPROTO_IPIP as i64),
    ("IPPROTO_IPV6", libc::IPPROTO_IPV6 as i64),
    ("IPPROTO_MAX", libc::IPPROTO_MAX as i64),
    ("IPPROTO_ND", libc::IPPROTO_ND as i64),
    ("IPPROTO_NONE", libc::IPPROTO_NONE as i64),
    ("IPPROTO_PIM", libc::IPPROTO_PIM as i64),
    ("IPPROTO_PUP", libc::IPPROTO_PUP as i64),
    ("IPPROTO_RAW", libc::IPPROTO_RAW as i64),
    ("IPPROTO_ROUTING", libc::IPPROTO_ROUTING as i64),
    ("IPPROTO_RSVP", libc::IPPROTO_RSVP as i64),
    ("IPPROTO_SCTP", libc::IPPROTO_SCTP as i64),
    ("IPPROTO_TCP", libc::IPPROTO_TCP as i64),
    ("IPPROTO_TP", libc::IPPROTO_TP as i64),
    ("IPPROTO_UDP", libc::IPPROTO_UDP as i64),
    ("IPPROTO_XTP", libc::IPPROTO_XTP as i64),
    ("IPV6_CHECKSUM", libc::IPV6_CHECKSUM as i64),
    ("IPV6_DONTFRAG", libc::IPV6_DONTFRAG as i64),
    ("IPV6_DSTOPTS", 50),
    ("IPV6_HOPLIMIT", libc::IPV6_HOPLIMIT as i64),
    ("IPV6_HOPOPTS", 49),
    ("IPV6_JOIN_GROUP", libc::IPV6_JOIN_GROUP as i64),
    ("IPV6_LEAVE_GROUP", libc::IPV6_LEAVE_GROUP as i64),
    ("IPV6_MULTICAST_HOPS", libc::IPV6_MULTICAST_HOPS as i64),
    ("IPV6_MULTICAST_IF", libc::IPV6_MULTICAST_IF as i64),
    ("IPV6_MULTICAST_LOOP", libc::IPV6_MULTICAST_LOOP as i64),
    ("IPV6_NEXTHOP", 48),
    ("IPV6_PATHMTU", 44),
    ("IPV6_PKTINFO", libc::IPV6_PKTINFO as i64),
    ("IPV6_RECVDSTOPTS", 40),
    ("IPV6_RECVHOPLIMIT", libc::IPV6_RECVHOPLIMIT as i64),
    ("IPV6_RECVHOPOPTS", 39),
    ("IPV6_RECVPATHMTU", 43),
    ("IPV6_RECVPKTINFO", libc::IPV6_RECVPKTINFO as i64),
    ("IPV6_RECVRTHDR", 38),
    ("IPV6_RECVTCLASS", libc::IPV6_RECVTCLASS as i64),
    ("IPV6_RTHDR", 51),
    ("IPV6_RTHDRDSTOPTS", 57),
    ("IPV6_RTHDR_TYPE_0", 0),
    ("IPV6_TCLASS", libc::IPV6_TCLASS as i64),
    ("IPV6_UNICAST_HOPS", libc::IPV6_UNICAST_HOPS as i64),
    ("IPV6_USE_MIN_MTU", 42),
    ("IPV6_V6ONLY", libc::IPV6_V6ONLY as i64),
    ("IP_ADD_MEMBERSHIP", libc::IP_ADD_MEMBERSHIP as i64),
    ("IP_ADD_SOURCE_MEMBERSHIP", libc::IP_ADD_SOURCE_MEMBERSHIP as i64),
    ("IP_BLOCK_SOURCE", libc::IP_BLOCK_SOURCE as i64),
    ("IP_DEFAULT_MULTICAST_LOOP", 1),
    ("IP_DEFAULT_MULTICAST_TTL", 1),
    ("IP_DROP_MEMBERSHIP", libc::IP_DROP_MEMBERSHIP as i64),
    ("IP_DROP_SOURCE_MEMBERSHIP", libc::IP_DROP_SOURCE_MEMBERSHIP as i64),
    ("IP_HDRINCL", libc::IP_HDRINCL as i64),
    ("IP_MAX_MEMBERSHIPS", 4095),
    ("IP_MULTICAST_IF", libc::IP_MULTICAST_IF as i64),
    ("IP_MULTICAST_LOOP", libc::IP_MULTICAST_LOOP as i64),
    ("IP_MULTICAST_TTL", libc::IP_MULTICAST_TTL as i64),
    ("IP_OPTIONS", 1),
    ("IP_PKTINFO", libc::IP_PKTINFO as i64),
    ("IP_RECVDSTADDR", libc::IP_RECVDSTADDR as i64),
    ("IP_RECVOPTS", 5),
    ("IP_RECVRETOPTS", 6),
    ("IP_RECVTOS", libc::IP_RECVTOS as i64),
    ("IP_RECVTTL", libc::IP_RECVTTL as i64),
    ("IP_RETOPTS", 8),
    ("IP_TOS", libc::IP_TOS as i64),
    ("IP_TTL", libc::IP_TTL as i64),
    ("IP_UNBLOCK_SOURCE", libc::IP_UNBLOCK_SOURCE as i64),
    ("LOCAL_PEERCRED", libc::LOCAL_PEERCRED as i64),
    ("MSG_CTRUNC", libc::MSG_CTRUNC as i64),
    ("MSG_DONTROUTE", libc::MSG_DONTROUTE as i64),
    ("MSG_DONTWAIT", libc::MSG_DONTWAIT as i64),
    ("MSG_EOF", libc::MSG_EOF as i64),
    ("MSG_EOR", libc::MSG_EOR as i64),
    ("MSG_NOSIGNAL", libc::MSG_NOSIGNAL as i64),
    ("MSG_OOB", libc::MSG_OOB as i64),
    ("MSG_PEEK", libc::MSG_PEEK as i64),
    ("MSG_TRUNC", libc::MSG_TRUNC as i64),
    ("MSG_WAITALL", libc::MSG_WAITALL as i64),
    ("NI_DGRAM", libc::NI_DGRAM as i64),
    ("NI_MAXHOST", libc::NI_MAXHOST as i64),
    ("NI_MAXSERV", libc::NI_MAXSERV as i64),
    ("NI_NAMEREQD", libc::NI_NAMEREQD as i64),
    ("NI_NOFQDN", libc::NI_NOFQDN as i64),
    ("NI_NUMERICHOST", libc::NI_NUMERICHOST as i64),
    ("NI_NUMERICSERV", libc::NI_NUMERICSERV as i64),
    ("PF_SYSTEM", libc::PF_SYSTEM as i64),
    ("SCM_CREDS", libc::SCM_CREDS as i64),
    ("SCM_RIGHTS", libc::SCM_RIGHTS as i64),
    ("SHUT_RD", libc::SHUT_RD as i64),
    ("SHUT_RDWR", libc::SHUT_RDWR as i64),
    ("SHUT_WR", libc::SHUT_WR as i64),
    ("SOCK_DGRAM", libc::SOCK_DGRAM as i64),
    ("SOCK_RAW", libc::SOCK_RAW as i64),
    ("SOCK_RDM", libc::SOCK_RDM as i64),
    ("SOCK_SEQPACKET", libc::SOCK_SEQPACKET as i64),
    ("SOCK_STREAM", libc::SOCK_STREAM as i64),
    ("SOL_IP", 0),
    ("SOL_SOCKET", libc::SOL_SOCKET as i64),
    ("SOL_TCP", libc::IPPROTO_TCP as i64),
    ("SOL_UDP", libc::IPPROTO_UDP as i64),
    ("SOMAXCONN", libc::SOMAXCONN as i64),
    ("SO_ACCEPTCONN", libc::SO_ACCEPTCONN as i64),
    ("SO_BINDTODEVICE", 0x1134),
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
    ("SO_USELOOPBACK", libc::SO_USELOOPBACK as i64),
    ("SYSPROTO_CONTROL", libc::SYSPROTO_CONTROL as i64),
    ("TCP_CONNECTION_INFO", libc::TCP_CONNECTION_INFO as i64),
    ("TCP_FASTOPEN", libc::TCP_FASTOPEN as i64),
    #[cfg(target_os = "macos")]
    ("TCP_KEEPALIVE", libc::TCP_KEEPALIVE as i64),
    ("TCP_KEEPCNT", libc::TCP_KEEPCNT as i64),
    #[cfg(target_os = "linux")]
    ("TCP_KEEPIDLE", libc::TCP_KEEPIDLE as i64),
    ("TCP_KEEPINTVL", libc::TCP_KEEPINTVL as i64),
    ("TCP_MAXSEG", libc::TCP_MAXSEG as i64),
    ("TCP_NODELAY", libc::TCP_NODELAY as i64),
    ("TCP_NOTSENT_LOWAT", 0x201),
];

/// Pure C helper functions that do not require `_socket.socket` fd ownership.
const REAL_FUNCTIONS: &[(&str, BuiltinFn)] = &[
    ("CMSG_LEN", socket_cmsg_len),
    ("CMSG_SPACE", socket_cmsg_space),
    ("if_indextoname", socket_if_indextoname),
    ("if_nameindex", socket_if_nameindex),
    ("if_nametoindex", socket_if_nametoindex),
    ("inet_ntop", socket_inet_ntop),
    ("inet_pton", socket_inet_pton),
    ("sethostname", socket_sethostname),
];

/// Resolver and byte-order entry points that still need a live network stack;
/// each raises `OSError` honestly when called.  `socketpair` is deliberately
/// NOT here so `socket.py` keeps using `_fallback_socketpair`.
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

    for &(function_name, entry) in REAL_FUNCTIONS.iter().chain(NOT_WIRED_FUNCTIONS.iter()) {
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
// Stateless host socket helpers

/// `socket.CMSG_LEN(length)`: cmsghdr plus payload, without trailing padding.
unsafe extern "C" fn socket_cmsg_len(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let length = match single_nonnegative_usize(argv, argc, "CMSG_LEN") {
        Ok(length) => length,
        Err(error) => return error,
    };
    let Some(total) = std::mem::size_of::<libc::cmsghdr>().checked_add(length) else {
        return raise_overflow_error("CMSG_LEN() argument out of range");
    };
    // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
    unsafe { abi::pon_const_int(total as i64) }
}

/// `socket.CMSG_SPACE(length)`: cmsghdr and payload rounded to CMSG alignment.
unsafe extern "C" fn socket_cmsg_space(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let length = match single_nonnegative_usize(argv, argc, "CMSG_SPACE") {
        Ok(length) => length,
        Err(error) => return error,
    };
    let Some(total) = cmsg_align(std::mem::size_of::<libc::cmsghdr>()).checked_add(cmsg_align(length)) else {
        return raise_overflow_error("CMSG_SPACE() argument out of range");
    };
    // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
    unsafe { abi::pon_const_int(total as i64) }
}

fn cmsg_align(length: usize) -> usize {
    let align = std::mem::size_of::<libc::c_long>();
    (length + align - 1) & !(align - 1)
}

unsafe extern "C" fn socket_inet_pton(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("inet_pton received a NULL argv pointer");
    };
    if args.len() != 2 {
        return raise_type_error("inet_pton() takes exactly 2 arguments");
    }
    let family = match int_arg(args[0], "inet_pton family") {
        Ok(family) => family as libc::c_int,
        Err(error) => return error,
    };
    let address = match text_or_bytes_arg(args[1], "inet_pton address") {
        Ok(address) => address,
        Err(error) => return error,
    };
    let c_address = match CString::new(address) {
        Ok(address) => address,
        Err(_) => return raise_type_error("inet_pton() argument 2 must not contain null bytes"),
    };
    let mut storage = [0u8; 16];
    let out_len = match family {
        libc::AF_INET => 4,
        libc::AF_INET6 => 16,
        _ => return super::os::raise_errno(libc::EAFNOSUPPORT, None),
    };
    // SAFETY: `c_address` is NUL-terminated and `storage` is large enough for
    // both supported address families.
    let rc = unsafe { inet_pton(family, c_address.as_ptr(), storage.as_mut_ptr().cast()) };
    match rc {
        1 => unsafe { abi::str_::pon_const_bytes(storage.as_ptr(), out_len) },
        0 => unsafe {
            let message = b"illegal IP address string passed to inet_pton";
            abi::exc::pon_raise_os_error(message.as_ptr(), message.len())
        },
        _ => super::os::raise_errno(last_errno(), None),
    }
}

unsafe extern "C" fn socket_inet_ntop(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("inet_ntop received a NULL argv pointer");
    };
    if args.len() != 2 {
        return raise_type_error("inet_ntop() takes exactly 2 arguments");
    }
    let family = match int_arg(args[0], "inet_ntop family") {
        Ok(family) => family as libc::c_int,
        Err(error) => return error,
    };
    let packed = untag(args[1]);
    let payload = match readable_bytes_payload(packed) {
        Ok(payload) => payload,
        Err(error) => return error,
    };
    let expected = match family {
        libc::AF_INET => 4,
        libc::AF_INET6 => 16,
        _ => return super::os::raise_errno(libc::EAFNOSUPPORT, None),
    };
    if payload.len() != expected {
        return unsafe {
            let message = b"packed IP wrong length for inet_ntop";
            abi::exc::pon_raise_value_error(message.as_ptr(), message.len())
        };
    }
    let mut dst = [0 as libc::c_char; INET6_ADDRSTRLEN];
    // SAFETY: Pointers reference live buffers sized for the requested family.
    let ptr = unsafe { inet_ntop(family, payload.as_ptr().cast(), dst.as_mut_ptr(), dst.len() as libc::socklen_t) };
    if ptr.is_null() {
        return super::os::raise_errno(last_errno(), None);
    }
    // SAFETY: `inet_ntop` wrote a NUL-terminated presentation string.
    let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
    alloc_str_object(&text)
}

unsafe extern "C" fn socket_if_nametoindex(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("if_nametoindex received a NULL argv pointer");
    };
    if args.len() != 1 {
        return raise_type_error("if_nametoindex() takes exactly one argument");
    }
    let name = match text_or_bytes_arg(args[0], "if_nametoindex name") {
        Ok(name) => name,
        Err(error) => return error,
    };
    let c_name = match CString::new(name) {
        Ok(name) => name,
        Err(_) => return raise_type_error("if_nametoindex() argument must not contain null bytes"),
    };
    // SAFETY: `c_name` is NUL-terminated.
    let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
    if index == 0 {
        return super::os::raise_errno(last_errno(), None);
    }
    unsafe { abi::pon_const_int(i64::from(index)) }
}

unsafe extern "C" fn socket_if_indextoname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let index = match single_nonnegative_usize(argv, argc, "if_indextoname") {
        Ok(index) => index as libc::c_uint,
        Err(error) => return error,
    };
    let mut name = [0 as libc::c_char; libc::IF_NAMESIZE as usize];
    // SAFETY: `name` is a writable IF_NAMESIZE-byte output buffer.
    let ptr = unsafe { libc::if_indextoname(index, name.as_mut_ptr()) };
    if ptr.is_null() {
        return super::os::raise_errno(last_errno(), None);
    }
    // SAFETY: libc wrote a NUL-terminated interface name.
    let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
    alloc_str_object(&text)
}

unsafe extern "C" fn socket_if_nameindex(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("if_nameindex() takes no arguments");
    }
    // SAFETY: libc allocates a NULL/zero terminated array.
    let entries = unsafe { libc::if_nameindex() };
    if entries.is_null() {
        return super::os::raise_errno(last_errno(), None);
    }
    let mut tuples: Vec<*mut PyObject> = Vec::new();
    let mut offset = 0usize;
    loop {
        // SAFETY: The array is terminated by an item with index 0 and NULL name.
        let entry = unsafe { &*entries.add(offset) };
        if entry.if_index == 0 && entry.if_name.is_null() {
            break;
        }
        // SAFETY: `if_name` is NUL-terminated for each live entry.
        let name = unsafe { CStr::from_ptr(entry.if_name) }.to_string_lossy();
        let mut pair = [unsafe { abi::pon_const_int(i64::from(entry.if_index)) }, alloc_str_object(&name)];
        if pair.iter().any(|value| value.is_null()) {
            unsafe { libc::if_freenameindex(entries) };
            return ptr::null_mut();
        }
        // SAFETY: Pair holds two live objects.
        let tuple = unsafe { abi::seq::pon_build_tuple(pair.as_mut_ptr(), pair.len()) };
        if tuple.is_null() {
            unsafe { libc::if_freenameindex(entries) };
            return ptr::null_mut();
        }
        tuples.push(tuple);
        offset += 1;
    }
    // SAFETY: Release libc's allocation once all Python strings are copied.
    unsafe { libc::if_freenameindex(entries) };
    // SAFETY: `tuples` holds live tuple objects.
    unsafe { abi::seq::pon_build_list(tuples.as_mut_ptr(), tuples.len()) }
}

unsafe extern "C" fn socket_sethostname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("sethostname received a NULL argv pointer");
    };
    if args.len() != 1 {
        return raise_type_error("sethostname() takes exactly one argument");
    }
    let name = match text_or_bytes_arg(args[0], "sethostname name") {
        Ok(name) => name,
        Err(error) => return error,
    };
    if name.contains(&0) {
        return raise_type_error("sethostname() argument must not contain null bytes");
    }
    // SAFETY: `name` points to `len` initialized bytes; Darwin's sethostname
    // length is explicit and does not require NUL termination.
    let rc = unsafe { libc::sethostname(name.as_ptr().cast(), name.len() as libc::c_int) };
    if rc < 0 {
        return super::os::raise_errno(last_errno(), None);
    }
    unsafe { abi::pon_none() }
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

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    if crate::tag::is_small_int(object) {
        return Ok(crate::tag::untag_small_int(object));
    }
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => value.to_i64().ok_or_else(|| raise_overflow_error(&format!("{what} is too large to fit in a C integer"))),
        None => Err(raise_type_error(&format!("{what} must be an integer"))),
    }
}

fn single_nonnegative_usize(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<usize, *mut PyObject> {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return Err(fail(format!("{name} received a NULL argv pointer")));
    };
    if args.len() != 1 {
        return Err(raise_type_error(&format!("{name}() takes exactly one argument")));
    }
    let value = int_arg(args[0], name)?;
    if value < 0 {
        return Err(raise_overflow_error(&format!("{name}() argument must be non-negative")));
    }
    Ok(value as usize)
}

fn text_or_bytes_arg(object: *mut PyObject, what: &str) -> Result<Vec<u8>, *mut PyObject> {
    let raw = untag(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(raise_type_error(&format!("{what} must be str or bytes")));
    }
    if let Some(text) = unsafe { type_mod::unicode_text(raw) } {
        return Ok(text.as_bytes().to_vec());
    }
    if let Some(bytes) = bytes_payload(raw) {
        return Ok(bytes.to_vec());
    }
    Err(raise_type_error(&format!("{what} must be str or bytes")))
}

fn bytes_payload<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytes_::is_bytes_type(ty) {
        Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() })
    } else if crate::types::bytearray_::is_bytearray_type(ty) {
        Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() })
    } else {
        None
    }
}

fn readable_bytes_payload<'a>(object: *mut PyObject) -> Result<&'a [u8], *mut PyObject> {
    if let Some(payload) = bytes_payload(object) {
        return Ok(payload);
    }
    if object.is_null() || crate::tag::is_small_int(object) {
        return Err(raise_type_error("a bytes-like object is required"));
    }
    let ty = unsafe { (*object).ob_type };
    if crate::types::memoryview::is_memoryview_type(ty) {
        let view = unsafe { &*object.cast::<crate::types::memoryview::PyMemoryView>() };
        if view.released {
            return Err(unsafe {
                abi::exc::pon_raise_value_error(
                    crate::types::memoryview::RELEASED_ERROR.as_ptr(),
                    crate::types::memoryview::RELEASED_ERROR.len(),
                )
            });
        }
        return Ok(unsafe { view.as_slice() });
    }
    Err(raise_type_error("a bytes-like object is required"))
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let object = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _socket.{name}"))
}
