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

use std::{
	ffi::{CStr, CString},
	ptr,
	sync::{LazyLock, Mutex},
};

use num_traits::ToPrimitive;

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, pon_err_message, pon_err_set},
	types::{
		exc::ExceptionKind,
		type_::{self as type_mod},
	},
};

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
	("ETHERTYPE_IPV6", 0x86dd),
	("ETHERTYPE_VLAN", 0x8100),
	("INADDR_ALLHOSTS_GROUP", 0xe0000001),
	("INADDR_ANY", libc::INADDR_ANY as i64),
	("INADDR_BROADCAST", libc::INADDR_BROADCAST as i64),
	("INADDR_LOOPBACK", libc::INADDR_LOOPBACK as i64),
	("INADDR_MAX_LOCAL_GROUP", 0xe00000ff),
	("INADDR_NONE", libc::INADDR_NONE as i64),
	("INADDR_UNSPEC_GROUP", 0xe0000000),
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

/// Host-backed module functions: address helpers, the live `socketpair(2)`
/// seed (asyncio's self-pipe), the blocking `getaddrinfo(3)` resolver, and
/// the byte-order primitives.
const REAL_FUNCTIONS: &[(&str, BuiltinFn)] = &[
	("CMSG_LEN", socket_cmsg_len),
	("CMSG_SPACE", socket_cmsg_space),
	("getaddrinfo", socket_getaddrinfo_real),
	("gethostname", socket_gethostname_real),
	("htonl", socket_htonl_real),
	("htons", socket_htons_real),
	("if_indextoname", socket_if_indextoname),
	("if_nameindex", socket_if_nameindex),
	("if_nametoindex", socket_if_nametoindex),
	("inet_ntop", socket_inet_ntop),
	("inet_pton", socket_inet_pton),
	("ntohl", socket_ntohl_real),
	("ntohs", socket_ntohs_real),
	("sethostname", socket_sethostname),
	("socketpair", socket_socketpair),
];

/// Entry points that still need work; each raises `OSError` honestly when
/// called.
const NOT_WIRED_FUNCTIONS: &[(&str, BuiltinFn)] = &[
	("close", socket_close),
	("dup", socket_dup),
	("gethostbyaddr", socket_gethostbyaddr),
	("gethostbyname", socket_gethostbyname),
	("gethostbyname_ex", socket_gethostbyname_ex),
	("getnameinfo", socket_getnameinfo),
	("getprotobyname", socket_getprotobyname),
	("getservbyname", socket_getservbyname),
	("getservbyport", socket_getservbyport),
	("inet_aton", socket_inet_aton),
	("inet_ntoa", socket_inet_ntoa),
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

static GAIERROR_CLASS: LazyLock<usize> =
	LazyLock::new(|| exception_class("gaierror", "OSError").map_or(0, |class| class as usize));

static HERROR_CLASS: LazyLock<usize> =
	LazyLock::new(|| exception_class("herror", "OSError").map_or(0, |class| class as usize));

/// Builds one `socket` exception heap class deriving from the named builtin,
/// with `__module__` set — CPython names these
/// `socket.gaierror`/`socket.herror`.
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
// A minimal LIVE fd wrapper: enough surface for `socket.socketpair()` and
// asyncio's self-pipe (create/adopt an fd, blocking control, send/recv,
// close/detach). Networking calls that need address marshalling
// (connect/bind/accept/getsockname/...) are still absent, so `socket.py`'s
// richer paths fail loudly at the attribute lookup. State lives in a side
// table keyed by instance address: the same functions serve both raw
// `_socket.socket` boxes and `socket.socket` heap-subclass instances.

/// Raw `_socket.socket` instance payload: header only, state in [`LIVE_SOCKETS`].
#[repr(C)]
struct PySocket {
	ob_base: PyObjectHeader,
}

/// Per-instance live-socket state, keyed by untagged instance address.
struct SockState {
	fd:      i32,
	family:  i64,
	kind:    i64,
	proto:   i64,
	timeout: Option<f64>,
}

static LIVE_SOCKETS: LazyLock<Mutex<std::collections::HashMap<usize, SockState>>> =
	LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

fn live_sockets() -> std::sync::MutexGuard<'static, std::collections::HashMap<usize, SockState>> {
	LIVE_SOCKETS.lock().unwrap_or_else(|poison| poison.into_inner())
}

static SOCKET_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "socket", std::mem::size_of::<PySocket>());
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(socket_new);
	// socket.py's `class socket(_socket.socket)` resolves `__init__` and the
	// fd methods through the type namespace; `family`/`type`/`proto` are
	// read-only properties, as in CPython.
	let namespace = type_mod::new_namespace();
	if !namespace.is_null() {
		let methods: &[(&str, BuiltinFn)] = &[
			("__init__", socket_init_method),
			("fileno", socket_fileno_method),
			("detach", socket_detach_method),
			("close", socket_close_method),
			("setblocking", socket_setblocking_method),
			("settimeout", socket_settimeout_method),
			("gettimeout", socket_gettimeout_method),
			("send", socket_send_method),
			("sendall", socket_sendall_method),
			("recv", socket_recv_method),
			("connect", socket_connect_method),
			("bind", socket_bind_method),
			("listen", socket_listen_method),
			("_accept", socket_accept_method),
			("getsockname", socket_getsockname_method),
			("getpeername", socket_getpeername_method),
			("setsockopt", socket_setsockopt_method),
			("getsockopt", socket_getsockopt_method),
			("shutdown", socket_shutdown_method),
		];
		for &(name, entry) in methods {
			// SAFETY: `entry` is a live builtin entry point with the runtime
			// calling convention.
			let function =
				unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
			if !function.is_null() {
				// SAFETY: `new_namespace` returned a live namespace box.
				unsafe { (*namespace).set(intern(name), function) };
			}
		}
		let properties: &[(&str, BuiltinFn)] = &[
			("family", socket_family_getter),
			("type", socket_type_getter),
			("proto", socket_proto_getter),
		];
		for &(name, entry) in properties {
			// SAFETY: `entry` is a live builtin entry point with the runtime
			// calling convention.
			let getter =
				unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
			if getter.is_null() {
				continue;
			}
			let descriptor = unsafe {
				crate::types::property::new_property(
					crate::native::builtins_mod::property_type().cast_const(),
					getter,
					ptr::null_mut(),
					ptr::null_mut(),
					ptr::null_mut(),
				)
			};
			if !descriptor.is_null() {
				// SAFETY: `new_namespace` returned a live namespace box.
				unsafe { (*namespace).set(intern(name), descriptor) };
			}
		}
		ty.tp_dict = namespace.cast::<PyObject>();
	}
	let ty = Box::into_raw(Box::new(ty));
	// GC rooting for the namespace's function objects.
	crate::sync::register_namespaced_type(ty);
	ty as usize
});

fn socket_type() -> *mut PyType {
	*SOCKET_TYPE as *mut PyType
}

fn runtime_object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

/// Parses one `__init__`/`socket_new` argument as i64 (default when absent
/// or None).
fn int_arg_or(args: &[*mut PyObject], index: usize, default: i64) -> Result<i64, *mut PyObject> {
	let Some(value) = args.get(index).copied() else {
		return Ok(default);
	};
	let value = untag(value);
	if value == unsafe { abi::pon_none() } {
		return Ok(default);
	}
	unsafe { crate::types::int::to_bigint_including_bool(value) }
		.and_then(|big| big.to_i64())
		.ok_or_else(|| raise_type_error("an integer is required"))
}

/// Creates or adopts the host fd for a construction call and records state.
fn socket_construct(receiver: *mut PyObject, args: &[*mut PyObject]) -> Result<(), *mut PyObject> {
	let family = int_arg_or(args, 0, libc::AF_INET as i64)?;
	let family = if family == -1 { libc::AF_INET as i64 } else { family };
	let kind = int_arg_or(args, 1, libc::SOCK_STREAM as i64)?;
	let kind = if kind == -1 { libc::SOCK_STREAM as i64 } else { kind };
	let proto = int_arg_or(args, 2, 0)?;
	let proto = if proto == -1 { 0 } else { proto };
	let fileno = int_arg_or(args, 3, -1)?;
	let fd = if fileno >= 0 {
		fileno as i32
	} else {
		// SAFETY: Plain socket(2); the result is validated below.
		let fd = unsafe { libc::socket(family as i32, kind as i32, proto as i32) };
		if fd < 0 {
			return Err(crate::native::os::raise_errno(last_socket_errno(), None));
		}
		fd
	};
	live_sockets().insert(untag(receiver) as usize, SockState {
		fd,
		family,
		kind,
		proto,
		timeout: None,
	});
	Ok(())
}

fn last_socket_errno() -> i32 {
	std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO)
}

/// `_socket.socket(family, type, proto, fileno)` direct construction (the
/// base type never routes through Python `__init__`).
unsafe extern "C" fn socket_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_mod::positional_args_from_object(args) } {
		Ok(args) => args,
		Err(message) => {
			pon_err_set(message);
			return ptr::null_mut();
		},
	};
	let object = Box::into_raw(Box::new(PySocket {
		ob_base: PyObjectHeader::new(socket_type().cast_const()),
	}))
	.cast::<PyObject>();
	match socket_construct(object, &positional) {
		Ok(()) => object,
		Err(raised) => raised,
	}
}

/// Receiver + argument split shared by every namespace method.
unsafe fn socket_method_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<(*mut PyObject, &'a [*mut PyObject]), *mut PyObject> {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return Err(fail(format!("socket.{name} received a NULL argv pointer")));
	};
	if args.is_empty() {
		return Err(raise_type_error(&format!("socket.{name} requires a receiver")));
	}
	Ok((untag(args[0]), &args[1..]))
}

fn socket_state_fd(receiver: *mut PyObject, name: &str) -> Result<i32, *mut PyObject> {
	match live_sockets().get(&(receiver as usize)) {
		Some(state) if state.fd >= 0 => Ok(state.fd),
		Some(_) => Err(crate::native::os::raise_errno(libc::EBADF, None)),
		None => Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::OSError,
			&format!("socket.{name}: receiver has no live socket state"),
		)),
	}
}

unsafe extern "C" fn socket_init_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "__init__") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	match socket_construct(receiver, args) {
		// SAFETY: Singleton accessor.
		Ok(()) => unsafe { abi::pon_none() },
		Err(raised) => raised,
	}
}

unsafe extern "C" fn socket_fileno_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, "fileno") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let fd = live_sockets()
		.get(&(receiver as usize))
		.map_or(-1, |state| i64::from(state.fd));
	// SAFETY: Integer boxing helper.
	unsafe { abi::pon_const_int(fd) }
}

unsafe extern "C" fn socket_detach_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, "detach") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let fd = match live_sockets().get_mut(&(receiver as usize)) {
		Some(state) => std::mem::replace(&mut state.fd, -1),
		None => -1,
	};
	// SAFETY: Integer boxing helper.
	unsafe { abi::pon_const_int(i64::from(fd)) }
}

unsafe extern "C" fn socket_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, "close") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Some(state) = live_sockets().get_mut(&(receiver as usize)) {
		let fd = std::mem::replace(&mut state.fd, -1);
		if fd >= 0 {
			// SAFETY: Plain close(2) on an fd this table owns.
			unsafe { libc::close(fd) };
		}
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_setblocking_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "setblocking") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() != 1 {
		return raise_type_error("setblocking() takes exactly one argument");
	}
	let blocking = match unsafe { crate::abstract_op::is_true(args[0]) } {
		1 => true,
		0 => false,
		_ => return ptr::null_mut(),
	};
	let fd = match socket_state_fd(receiver, "setblocking") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	if let Err(raised) = set_fd_blocking(fd, blocking) {
		return raised;
	}
	if let Some(state) = live_sockets().get_mut(&(receiver as usize)) {
		state.timeout = if blocking { None } else { Some(0.0) };
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

fn set_fd_blocking(fd: i32, blocking: bool) -> Result<(), *mut PyObject> {
	// SAFETY: Plain fcntl on a live fd; failures map to errno.
	let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
	if flags < 0 {
		return Err(crate::native::os::raise_errno(last_socket_errno(), None));
	}
	let flags = if blocking { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
	// SAFETY: Same fd, flag update only.
	if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
		return Err(crate::native::os::raise_errno(last_socket_errno(), None));
	}
	Ok(())
}

unsafe extern "C" fn socket_settimeout_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "settimeout") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() != 1 {
		return raise_type_error("settimeout() takes exactly one argument");
	}
	let value = untag(args[0]);
	let timeout = if value == unsafe { abi::pon_none() } {
		None
	} else if let Some(seconds) = unsafe { crate::types::float::to_f64(value) } {
		Some(seconds)
	} else if let Some(seconds) =
		unsafe { crate::types::int::to_bigint(value) }.and_then(|big| num_bigint_to_f64(&big))
	{
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
	let fd = match socket_state_fd(receiver, "settimeout") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// CPython: a zero timeout is non-blocking mode; None restores blocking.
	if let Err(raised) = set_fd_blocking(fd, timeout.is_none()) {
		return raised;
	}
	if let Some(state) = live_sockets().get_mut(&(receiver as usize)) {
		state.timeout = timeout;
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_gettimeout_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, "gettimeout") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let timeout = live_sockets()
		.get(&(receiver as usize))
		.and_then(|state| state.timeout);
	match timeout {
		// SAFETY: Runtime allocation helper; NULL propagates with the error set.
		Some(seconds) => unsafe { abi::number::pon_const_float(seconds) },
		// SAFETY: Singleton accessor.
		None => unsafe { abi::pon_none() },
	}
}

unsafe extern "C" fn socket_send_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "send") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("send() takes 1 or 2 arguments");
	}
	let Ok(data) = crate::abi::str_::expect_bytes_like(untag(args[0])) else {
		return raise_type_error("a bytes-like object is required");
	};
	let fd = match socket_state_fd(receiver, "send") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// SAFETY: Plain send(2) over a validated fd and an owned byte buffer.
	let sent = unsafe { libc::send(fd, data.as_ptr().cast(), data.len(), 0) };
	if sent < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Integer boxing helper.
	unsafe { abi::pon_const_int(sent as i64) }
}

unsafe extern "C" fn socket_sendall_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "sendall") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("sendall() takes 1 or 2 arguments");
	}
	let Ok(data) = crate::abi::str_::expect_bytes_like(untag(args[0])) else {
		return raise_type_error("a bytes-like object is required");
	};
	let fd = match socket_state_fd(receiver, "sendall") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let mut offset = 0usize;
	while offset < data.len() {
		// SAFETY: Plain send(2) over a validated fd and an owned byte buffer.
		let sent = unsafe { libc::send(fd, data[offset..].as_ptr().cast(), data.len() - offset, 0) };
		if sent < 0 {
			return crate::native::os::raise_errno(last_socket_errno(), None);
		}
		offset += sent as usize;
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_recv_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "recv") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("recv() takes 1 or 2 arguments");
	}
	let size = match int_arg_or(args, 0, -1) {
		Ok(size) if size >= 0 => size as usize,
		Ok(_) => return raise_type_error("negative buffersize in recv"),
		Err(raised) => return raised,
	};
	let fd = match socket_state_fd(receiver, "recv") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let mut buffer = vec![0u8; size.max(1)];
	// SAFETY: Plain recv(2) into an owned buffer of the requested size.
	let received = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), size, 0) };
	if received < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	buffer.truncate(received as usize);
	// SAFETY: Runtime allocation helper; NULL propagates with the error set.
	unsafe { abi::str_::pon_const_bytes(buffer.as_ptr(), buffer.len()) }
}

fn socket_state_property(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	read: fn(&SockState) -> i64,
) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, name) } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let value = live_sockets()
		.get(&(receiver as usize))
		.map_or(-1, read);
	// SAFETY: Integer boxing helper.
	unsafe { abi::pon_const_int(value) }
}

unsafe extern "C" fn socket_family_getter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	socket_state_property(argv, argc, "family", |state| state.family)
}

unsafe extern "C" fn socket_type_getter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	socket_state_property(argv, argc, "type", |state| state.kind)
}

unsafe extern "C" fn socket_proto_getter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	socket_state_property(argv, argc, "proto", |state| state.proto)
}

/// `_socket.socketpair(family=AF_UNIX, type=SOCK_STREAM, proto=0)`: a real
/// `socketpair(2)`, returned as two raw `_socket.socket` objects that
/// `socket.py` re-wraps through `detach()`.
unsafe extern "C" fn socket_socketpair(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("socketpair received a NULL argv pointer");
	};
	let family = match int_arg_or(args, 0, libc::AF_UNIX as i64) {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let kind = match int_arg_or(args, 1, libc::SOCK_STREAM as i64) {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let proto = match int_arg_or(args, 2, 0) {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let mut fds = [0i32; 2];
	// SAFETY: Plain socketpair(2) writing two fds into the stack array.
	if unsafe { libc::socketpair(family as i32, kind as i32, proto as i32, fds.as_mut_ptr()) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	let mut pair = [ptr::null_mut::<PyObject>(); 2];
	for (slot, fd) in pair.iter_mut().zip(fds) {
		let object = Box::into_raw(Box::new(PySocket {
			ob_base: PyObjectHeader::new(socket_type().cast_const()),
		}))
		.cast::<PyObject>();
		live_sockets().insert(object as usize, SockState {
			fd,
			family,
			kind,
			proto,
			timeout: None,
		});
		*slot = object;
	}
	// SAFETY: Two live socket objects; the tuple allocator copies the slots.
	unsafe { crate::abi::seq::pon_build_tuple(pair.as_mut_ptr(), 2) }
}

// ---------------------------------------------------------------------------
// Address marshalling (AF_INET / AF_INET6 / AF_UNIX)

/// A filled `sockaddr_storage` plus its meaningful length.
struct HostAddress {
	storage: libc::sockaddr_storage,
	len:     libc::socklen_t,
}

impl HostAddress {
	fn as_sockaddr(&self) -> *const libc::sockaddr {
		(&raw const self.storage).cast()
	}
}

/// Parses a Python address argument (`(host, port)` tuples for INET
/// families, a path string for AF_UNIX) into a host sockaddr. Non-numeric
/// INET hosts resolve through `getaddrinfo(3)`.
fn parse_address(family: i64, object: *mut PyObject) -> Result<HostAddress, *mut PyObject> {
	let object = untag(object);
	// SAFETY: Zeroed sockaddr_storage is a valid all-families baseline.
	let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
	match family as i32 {
		libc::AF_UNIX => {
			let Some(path) = (unsafe { type_mod::unicode_text(object) }) else {
				return Err(raise_type_error("AF_UNIX address must be str"));
			};
			let sun = (&raw mut storage).cast::<libc::sockaddr_un>();
			// SAFETY: `sockaddr_un` fits inside `sockaddr_storage`.
			unsafe {
				(*sun).sun_family = libc::AF_UNIX as libc::sa_family_t;
				let bytes = path.as_bytes();
				if bytes.len() >= (*sun).sun_path.len() {
					return Err(raise_os_error_text("AF_UNIX path too long"));
				}
				for (slot, byte) in (*sun).sun_path.iter_mut().zip(bytes) {
					*slot = *byte as libc::c_char;
				}
				(*sun).sun_len = (std::mem::size_of::<libc::sockaddr_un>()) as u8;
			}
			Ok(HostAddress {
				storage,
				len: std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
			})
		},
		libc::AF_INET | libc::AF_INET6 => {
			let Some(items) = (unsafe { crate::abi::seq::exact_tuple_slice(object) }) else {
				return Err(raise_type_error("socket address must be a (host, port) tuple"));
			};
			if items.len() < 2 {
				return Err(raise_type_error("socket address must be a (host, port) tuple"));
			}
			let Some(host) = (unsafe { type_mod::unicode_text(untag(items[0])) }) else {
				return Err(raise_type_error("socket address host must be str"));
			};
			let port = match int_arg_or(items, 1, -1) {
				Ok(port) if (0..=65535).contains(&port) => port as u16,
				Ok(_) => return Err(raise_os_error_text("port must be 0-65535")),
				Err(raised) => return Err(raised),
			};
			let host: &str = if host.is_empty() {
				if family as i32 == libc::AF_INET { "0.0.0.0" } else { "::" }
			} else if host == "<broadcast>" {
				"255.255.255.255"
			} else {
				host
			};
			resolve_host(family as i32, host, port)
		},
		other => Err(raise_os_error_text(&format!(
			"address family {other} is not supported by the pon runtime"
		))),
	}
}

/// Numeric fast path via `inet_pton(3)`, then a blocking `getaddrinfo(3)`
/// resolution for names.
fn resolve_host(family: i32, host: &str, port: u16) -> Result<HostAddress, *mut PyObject> {
	// SAFETY: Zeroed sockaddr_storage is a valid all-families baseline.
	let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
	let Ok(c_host) = CString::new(host) else {
		return Err(raise_type_error("socket address host contains NUL"));
	};
	if family == libc::AF_INET {
		let sin = (&raw mut storage).cast::<libc::sockaddr_in>();
		// SAFETY: `sockaddr_in` fits inside `sockaddr_storage`.
		let numeric = unsafe { inet_pton(libc::AF_INET, c_host.as_ptr(), (&raw mut (*sin).sin_addr).cast()) };
		if numeric == 1 {
			// SAFETY: Same live struct as above.
			unsafe {
				(*sin).sin_family = libc::AF_INET as libc::sa_family_t;
				(*sin).sin_port = port.to_be();
				(*sin).sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
			}
			return Ok(HostAddress {
				storage,
				len: std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
			});
		}
	} else {
		let sin6 = (&raw mut storage).cast::<libc::sockaddr_in6>();
		// SAFETY: `sockaddr_in6` fits inside `sockaddr_storage`.
		let numeric = unsafe { inet_pton(libc::AF_INET6, c_host.as_ptr(), (&raw mut (*sin6).sin6_addr).cast()) };
		if numeric == 1 {
			// SAFETY: Same live struct as above.
			unsafe {
				(*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
				(*sin6).sin6_port = port.to_be();
				(*sin6).sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
			}
			return Ok(HostAddress {
				storage,
				len: std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
			});
		}
	}
	// Name resolution: first getaddrinfo(3) record of the requested family.
	let records = getaddrinfo_records(Some(host), Some(&port.to_string()), family, 0, 0, 0)?;
	records
		.into_iter()
		.next()
		.map(|record| record.address)
		.ok_or_else(|| raise_os_error_text(&format!("no address records for host {host:?}")))
}

/// One `getaddrinfo(3)` record in CPython result order.
struct AddrInfoRecord {
	family:    i32,
	kind:      i32,
	proto:     i32,
	canonname: String,
	address:   HostAddress,
}

fn getaddrinfo_records(
	host: Option<&str>,
	service: Option<&str>,
	family: i32,
	kind: i32,
	proto: i32,
	flags: i32,
) -> Result<Vec<AddrInfoRecord>, *mut PyObject> {
	let c_host = match host {
		Some(host) => match CString::new(host) {
			Ok(c_host) => Some(c_host),
			Err(_) => return Err(raise_type_error("host contains NUL")),
		},
		None => None,
	};
	let c_service = match service {
		Some(service) => match CString::new(service) {
			Ok(c_service) => Some(c_service),
			Err(_) => return Err(raise_type_error("service contains NUL")),
		},
		None => None,
	};
	// SAFETY: Zeroed hints struct is the documented getaddrinfo(3) baseline.
	let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
	hints.ai_family = family;
	hints.ai_socktype = kind;
	hints.ai_protocol = proto;
	hints.ai_flags = flags;
	let mut result: *mut libc::addrinfo = ptr::null_mut();
	// SAFETY: All pointers are live for the call; result is freed below.
	let status = unsafe {
		libc::getaddrinfo(
			c_host.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
			c_service.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
			&hints,
			&mut result,
		)
	};
	if status != 0 {
		// SAFETY: gai_strerror returns a static message for the status code.
		let detail = unsafe { CStr::from_ptr(libc::gai_strerror(status)) }
			.to_string_lossy()
			.into_owned();
		return Err(raise_os_error_text(&format!("[Errno {status}] {detail}")));
	}
	let mut records = Vec::new();
	let mut cursor = result;
	while !cursor.is_null() {
		// SAFETY: The chain entries stay live until freeaddrinfo below.
		let entry = unsafe { &*cursor };
		// SAFETY: Zeroed baseline, then a bounded copy of ai_addrlen bytes.
		let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
		let len = entry
			.ai_addrlen
			.min(std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t);
		// SAFETY: `ai_addr` points at `ai_addrlen` readable bytes.
		unsafe {
			ptr::copy_nonoverlapping(entry.ai_addr.cast::<u8>(), (&raw mut storage).cast::<u8>(), len as usize);
		}
		let canonname = if entry.ai_canonname.is_null() {
			String::new()
		} else {
			// SAFETY: Non-null canonname is a NUL-terminated C string.
			unsafe { CStr::from_ptr(entry.ai_canonname) }
				.to_string_lossy()
				.into_owned()
		};
		records.push(AddrInfoRecord {
			family: entry.ai_family,
			kind: entry.ai_socktype,
			proto: entry.ai_protocol,
			canonname,
			address: HostAddress { storage, len },
		});
		cursor = entry.ai_next;
	}
	// SAFETY: `result` came from a successful getaddrinfo call.
	unsafe { libc::freeaddrinfo(result) };
	Ok(records)
}

/// Renders a sockaddr as CPython's Python-level address value:
/// `(host, port)` for AF_INET, `(host, port, flowinfo, scope_id)` for
/// AF_INET6, a path string for AF_UNIX.
fn address_to_object(address: &HostAddress) -> *mut PyObject {
	match i32::from(address.storage.ss_family) {
		libc::AF_INET => {
			let sin = (&raw const address.storage).cast::<libc::sockaddr_in>();
			let mut text = [0i8; INET6_ADDRSTRLEN];
			// SAFETY: Live sockaddr_in and a correctly sized text buffer.
			let rendered = unsafe {
				inet_ntop(
					libc::AF_INET,
					(&raw const (*sin).sin_addr).cast(),
					text.as_mut_ptr(),
					INET6_ADDRSTRLEN as libc::socklen_t,
				)
			};
			if rendered.is_null() {
				return crate::native::os::raise_errno(last_socket_errno(), None);
			}
			// SAFETY: inet_ntop wrote a NUL-terminated string.
			let host = unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy();
			// SAFETY: Live struct; port is stored big-endian.
			let port = u16::from_be(unsafe { (*sin).sin_port });
			let host_object = unsafe { abi::pon_const_str(host.as_ptr(), host.len()) };
			let port_object = unsafe { abi::pon_const_int(i64::from(port)) };
			if host_object.is_null() || port_object.is_null() {
				return ptr::null_mut();
			}
			let mut pair = [host_object, port_object];
			// SAFETY: Two live slots; the tuple allocator copies them.
			unsafe { crate::abi::seq::pon_build_tuple(pair.as_mut_ptr(), 2) }
		},
		libc::AF_INET6 => {
			let sin6 = (&raw const address.storage).cast::<libc::sockaddr_in6>();
			let mut text = [0i8; INET6_ADDRSTRLEN];
			// SAFETY: Live sockaddr_in6 and a correctly sized text buffer.
			let rendered = unsafe {
				inet_ntop(
					libc::AF_INET6,
					(&raw const (*sin6).sin6_addr).cast(),
					text.as_mut_ptr(),
					INET6_ADDRSTRLEN as libc::socklen_t,
				)
			};
			if rendered.is_null() {
				return crate::native::os::raise_errno(last_socket_errno(), None);
			}
			// SAFETY: inet_ntop wrote a NUL-terminated string.
			let host = unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy();
			// SAFETY: Live struct; port/flowinfo stored big-endian.
			let (port, flowinfo, scope) = unsafe {
				(
					u16::from_be((*sin6).sin6_port),
					u32::from_be((*sin6).sin6_flowinfo),
					(*sin6).sin6_scope_id,
				)
			};
			let host_object = unsafe { abi::pon_const_str(host.as_ptr(), host.len()) };
			let port_object = unsafe { abi::pon_const_int(i64::from(port)) };
			let flow_object = unsafe { abi::pon_const_int(i64::from(flowinfo)) };
			let scope_object = unsafe { abi::pon_const_int(i64::from(scope)) };
			if host_object.is_null() || port_object.is_null() || flow_object.is_null() || scope_object.is_null() {
				return ptr::null_mut();
			}
			let mut parts = [host_object, port_object, flow_object, scope_object];
			// SAFETY: Four live slots; the tuple allocator copies them.
			unsafe { crate::abi::seq::pon_build_tuple(parts.as_mut_ptr(), 4) }
		},
		libc::AF_UNIX => {
			let sun = (&raw const address.storage).cast::<libc::sockaddr_un>();
			// SAFETY: sun_path is NUL-terminated for bound/parsed addresses.
			let path = unsafe { CStr::from_ptr((*sun).sun_path.as_ptr()) }.to_string_lossy();
			unsafe { abi::pon_const_str(path.as_ptr(), path.len()) }
		},
		_ => {
			// Unknown family: CPython falls back to the raw bytes; an empty
			// tuple keeps consumers alive without inventing structure.
			unsafe { crate::abi::seq::pon_build_tuple(ptr::null_mut(), 0) }
		},
	}
}

fn raise_os_error_text(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::OSError, message)
}

// ---------------------------------------------------------------------------
// Connection-oriented socket methods

unsafe extern "C" fn socket_connect_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "connect") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() != 1 {
		return raise_type_error("connect() takes exactly one argument");
	}
	let (fd, family) = match live_sockets().get(&(receiver as usize)) {
		Some(state) if state.fd >= 0 => (state.fd, state.family),
		_ => return raise_os_error_text("connect on a closed or unregistered socket"),
	};
	let address = match parse_address(family, args[0]) {
		Ok(address) => address,
		Err(raised) => return raised,
	};
	// SAFETY: Plain connect(2) on a validated fd and a filled sockaddr.
	if unsafe { libc::connect(fd, address.as_sockaddr(), address.len) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_bind_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "bind") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() != 1 {
		return raise_type_error("bind() takes exactly one argument");
	}
	let (fd, family) = match live_sockets().get(&(receiver as usize)) {
		Some(state) if state.fd >= 0 => (state.fd, state.family),
		_ => return raise_os_error_text("bind on a closed or unregistered socket"),
	};
	let address = match parse_address(family, args[0]) {
		Ok(address) => address,
		Err(raised) => return raised,
	};
	// SAFETY: Plain bind(2) on a validated fd and a filled sockaddr.
	if unsafe { libc::bind(fd, address.as_sockaddr(), address.len) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_listen_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "listen") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let backlog = match int_arg_or(args, 0, 128) {
		Ok(backlog) => backlog,
		Err(raised) => return raised,
	};
	let fd = match socket_state_fd(receiver, "listen") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// SAFETY: Plain listen(2) on a validated fd.
	if unsafe { libc::listen(fd, backlog as i32) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

/// `_socket.socket._accept()`: CPython's raw accept — `(fd, address)`;
/// `socket.py` wraps the fd via `socket(..., fileno=fd)`.
unsafe extern "C" fn socket_accept_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, "_accept") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let fd = match socket_state_fd(receiver, "_accept") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// SAFETY: Zeroed sockaddr_storage receives the peer address in place.
	let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
	let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
	// SAFETY: Plain accept(2) writing into the storage above.
	let conn = unsafe { libc::accept(fd, (&raw mut storage).cast(), &mut len) };
	if conn < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	let address = address_to_object(&HostAddress { storage, len });
	if address.is_null() {
		// SAFETY: The freshly accepted fd is owned here until returned.
		unsafe { libc::close(conn) };
		return ptr::null_mut();
	}
	let fd_object = unsafe { abi::pon_const_int(i64::from(conn)) };
	if fd_object.is_null() {
		// SAFETY: Same ownership as above.
		unsafe { libc::close(conn) };
		return ptr::null_mut();
	}
	let mut pair = [fd_object, address];
	// SAFETY: Two live slots; the tuple allocator copies them.
	unsafe { crate::abi::seq::pon_build_tuple(pair.as_mut_ptr(), 2) }
}

fn socket_name_method(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	read: unsafe fn(i32, *mut libc::sockaddr, *mut libc::socklen_t) -> i32,
) -> *mut PyObject {
	let (receiver, _) = match unsafe { socket_method_args(argv, argc, name) } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let fd = match socket_state_fd(receiver, name) {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// SAFETY: Zeroed sockaddr_storage receives the address in place.
	let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
	let mut len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
	// SAFETY: Plain getsockname/getpeername on a validated fd.
	if unsafe { read(fd, (&raw mut storage).cast(), &mut len) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	address_to_object(&HostAddress { storage, len })
}

unsafe extern "C" fn socket_getsockname_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	socket_name_method(argv, argc, "getsockname", |fd, addr, len| unsafe {
		libc::getsockname(fd, addr, len)
	})
}

unsafe extern "C" fn socket_getpeername_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	socket_name_method(argv, argc, "getpeername", |fd, addr, len| unsafe {
		libc::getpeername(fd, addr, len)
	})
}

unsafe extern "C" fn socket_setsockopt_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "setsockopt") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() != 3 {
		return raise_type_error("setsockopt() takes exactly three arguments");
	}
	let fd = match socket_state_fd(receiver, "setsockopt") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let level = match int_arg_or(args, 0, -1) {
		Ok(level) => level as i32,
		Err(raised) => return raised,
	};
	let option = match int_arg_or(args, 1, -1) {
		Ok(option) => option as i32,
		Err(raised) => return raised,
	};
	// Value: int (the common case) or a bytes-like buffer.
	let status = if let Ok(buffer) = crate::abi::str_::expect_bytes_like(untag(args[2])) {
		// SAFETY: Plain setsockopt(2) over an owned buffer.
		unsafe {
			libc::setsockopt(fd, level, option, buffer.as_ptr().cast(), buffer.len() as libc::socklen_t)
		}
	} else {
		let value = match int_arg_or(args, 2, -1) {
			Ok(value) => value as i32,
			Err(raised) => return raised,
		};
		// SAFETY: Plain setsockopt(2) over a stack int.
		unsafe {
			libc::setsockopt(
				fd,
				level,
				option,
				(&raw const value).cast(),
				std::mem::size_of::<i32>() as libc::socklen_t,
			)
		}
	};
	if status < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn socket_getsockopt_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "getsockopt") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if args.len() < 2 || args.len() > 3 {
		return raise_type_error("getsockopt() takes two or three arguments");
	}
	let fd = match socket_state_fd(receiver, "getsockopt") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let level = match int_arg_or(args, 0, -1) {
		Ok(level) => level as i32,
		Err(raised) => return raised,
	};
	let option = match int_arg_or(args, 1, -1) {
		Ok(option) => option as i32,
		Err(raised) => return raised,
	};
	let buflen = match int_arg_or(args, 2, 0) {
		Ok(buflen) => buflen,
		Err(raised) => return raised,
	};
	if buflen == 0 {
		let mut value: i32 = 0;
		let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
		// SAFETY: Plain getsockopt(2) into a stack int.
		if unsafe { libc::getsockopt(fd, level, option, (&raw mut value).cast(), &mut len) } < 0 {
			return crate::native::os::raise_errno(last_socket_errno(), None);
		}
		// SAFETY: Integer boxing helper.
		return unsafe { abi::pon_const_int(i64::from(value)) };
	}
	if !(0..=1024).contains(&buflen) {
		return raise_os_error_text("getsockopt buflen out of range");
	}
	let mut buffer = vec![0u8; buflen as usize];
	let mut len = buflen as libc::socklen_t;
	// SAFETY: Plain getsockopt(2) into an owned buffer.
	if unsafe { libc::getsockopt(fd, level, option, buffer.as_mut_ptr().cast(), &mut len) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	buffer.truncate(len as usize);
	// SAFETY: Runtime allocation helper; NULL propagates with the error set.
	unsafe { abi::str_::pon_const_bytes(buffer.as_ptr(), buffer.len()) }
}

unsafe extern "C" fn socket_shutdown_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { socket_method_args(argv, argc, "shutdown") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let how = match int_arg_or(args, 0, libc::SHUT_RDWR as i64) {
		Ok(how) => how as i32,
		Err(raised) => return raised,
	};
	let fd = match socket_state_fd(receiver, "shutdown") {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	// SAFETY: Plain shutdown(2) on a validated fd.
	if unsafe { libc::shutdown(fd, how) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

/// `_socket.getaddrinfo(host, port, family=0, type=0, proto=0, flags=0)`.
unsafe extern "C" fn socket_getaddrinfo_real(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("getaddrinfo received a NULL argv pointer");
	};
	if args.len() < 2 {
		return raise_type_error("getaddrinfo() requires host and port");
	}
	let none = unsafe { abi::pon_none() };
	let host_object = untag(args[0]);
	let host = if host_object == none {
		None
	} else if let Some(text) = unsafe { type_mod::unicode_text(host_object) } {
		Some(text.to_owned())
	} else if let Ok(bytes) = crate::abi::str_::expect_bytes_like(host_object) {
		match String::from_utf8(bytes) {
			Ok(text) => Some(text),
			Err(_) => return raise_type_error("getaddrinfo host bytes must be UTF-8"),
		}
	} else {
		return raise_type_error("getaddrinfo() argument 1 must be string, bytes or None");
	};
	let port_object = untag(args[1]);
	let service = if port_object == none {
		None
	} else if let Some(text) = unsafe { type_mod::unicode_text(port_object) } {
		Some(text.to_owned())
	} else if let Some(port) =
		unsafe { crate::types::int::to_bigint_including_bool(port_object) }.and_then(|big| big.to_i64())
	{
		Some(port.to_string())
	} else {
		return raise_type_error("getaddrinfo() argument 2 must be integer, string or None");
	};
	let family = match int_arg_or(args, 2, 0) {
		Ok(value) => value as i32,
		Err(raised) => return raised,
	};
	let kind = match int_arg_or(args, 3, 0) {
		Ok(value) => value as i32,
		Err(raised) => return raised,
	};
	let proto = match int_arg_or(args, 4, 0) {
		Ok(value) => value as i32,
		Err(raised) => return raised,
	};
	let flags = match int_arg_or(args, 5, 0) {
		Ok(value) => value as i32,
		Err(raised) => return raised,
	};
	let records = match getaddrinfo_records(host.as_deref(), service.as_deref(), family, kind, proto, flags) {
		Ok(records) => records,
		Err(raised) => return raised,
	};
	let mut items = Vec::with_capacity(records.len());
	for record in &records {
		let family_object = unsafe { abi::pon_const_int(i64::from(record.family)) };
		let kind_object = unsafe { abi::pon_const_int(i64::from(record.kind)) };
		let proto_object = unsafe { abi::pon_const_int(i64::from(record.proto)) };
		let canon_object =
			unsafe { abi::pon_const_str(record.canonname.as_ptr(), record.canonname.len()) };
		let address = address_to_object(&record.address);
		if family_object.is_null()
			|| kind_object.is_null()
			|| proto_object.is_null()
			|| canon_object.is_null()
			|| address.is_null()
		{
			return ptr::null_mut();
		}
		let mut parts = [family_object, kind_object, proto_object, canon_object, address];
		// SAFETY: Five live slots; the tuple allocator copies them.
		let record_tuple = unsafe { crate::abi::seq::pon_build_tuple(parts.as_mut_ptr(), 5) };
		if record_tuple.is_null() {
			return ptr::null_mut();
		}
		items.push(record_tuple);
	}
	// SAFETY: Live tuple slots; the list allocator copies them.
	unsafe { crate::abi::seq::pon_build_list(items.as_mut_ptr(), items.len()) }
}

/// `_socket.gethostname()`.
unsafe extern "C" fn socket_gethostname_real(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return raise_type_error("gethostname() takes no arguments");
	}
	let mut buffer = [0i8; 256];
	// SAFETY: Plain gethostname(3) into a stack buffer.
	if unsafe { libc::gethostname(buffer.as_mut_ptr(), buffer.len()) } < 0 {
		return crate::native::os::raise_errno(last_socket_errno(), None);
	}
	// SAFETY: gethostname wrote a NUL-terminated string.
	let name = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
	unsafe { abi::pon_const_str(name.as_ptr(), name.len()) }
}

/// Shared 16/32-bit byte-swap core for `htons`/`ntohs`/`htonl`/`ntohl`
/// (identical swap on both directions for a big-endian wire format).
fn byteswap_entry(argv: *mut *mut PyObject, argc: usize, name: &str, wide: bool) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail(format!("{name} received a NULL argv pointer"));
	};
	if args.len() != 1 {
		return raise_type_error(&format!("{name}() takes exactly one argument"));
	}
	let value = match int_arg_or(args, 0, -1) {
		Ok(value) if value >= 0 => value,
		Ok(_) => return raise_os_error_text(&format!("{name}() argument out of range")),
		Err(raised) => return raised,
	};
	let swapped = if wide {
		if value > i64::from(u32::MAX) {
			return raise_os_error_text(&format!("{name}() argument out of range"));
		}
		i64::from((value as u32).swap_bytes())
	} else {
		if value > i64::from(u16::MAX) {
			return raise_os_error_text(&format!("{name}() argument out of range"));
		}
		i64::from((value as u16).swap_bytes())
	};
	let swapped = if cfg!(target_endian = "big") { value } else { swapped };
	// SAFETY: Integer boxing helper.
	unsafe { abi::pon_const_int(swapped) }
}

unsafe extern "C" fn socket_htons_real(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	byteswap_entry(argv, argc, "htons", false)
}

unsafe extern "C" fn socket_ntohs_real(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	byteswap_entry(argv, argc, "ntohs", false)
}

unsafe extern "C" fn socket_htonl_real(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	byteswap_entry(argv, argc, "htonl", true)
}

unsafe extern "C" fn socket_ntohl_real(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	byteswap_entry(argv, argc, "ntohl", true)
}

// ---------------------------------------------------------------------------
// Default-timeout state (real, not a stub: pure module state)

static DEFAULT_TIMEOUT: Mutex<Option<f64>> = Mutex::new(None);

unsafe extern "C" fn socket_getdefaulttimeout(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	let timeout = *DEFAULT_TIMEOUT
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	match timeout {
		// SAFETY: Runtime allocation helper; NULL propagates with the error set.
		Some(seconds) => unsafe { abi::number::pon_const_float(seconds) },
		// SAFETY: Singleton accessor.
		None => unsafe { abi::pon_none() },
	}
}

unsafe extern "C" fn socket_setdefaulttimeout(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("setdefaulttimeout received a NULL argv pointer");
	};
	if args.len() != 1 {
		let message =
			format!("setdefaulttimeout() takes exactly one argument ({} given)", args.len());
		return raise_type_error(&message);
	}
	let value = untag(args[0]);
	let timeout = if value == unsafe { abi::pon_none() } {
		None
	} else if let Some(seconds) = unsafe { crate::types::float::to_f64(value) } {
		Some(seconds)
	} else if let Some(seconds) =
		unsafe { crate::types::int::to_bigint(value) }.and_then(|big| num_bigint_to_f64(&big))
	{
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
	*DEFAULT_TIMEOUT
		.lock()
		.unwrap_or_else(|poison| poison.into_inner()) = timeout;
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
	let Some(total) =
		cmsg_align(std::mem::size_of::<libc::cmsghdr>()).checked_add(cmsg_align(length))
	else {
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
	let ptr = unsafe {
		inet_ntop(family, payload.as_ptr().cast(), dst.as_mut_ptr(), dst.len() as libc::socklen_t)
	};
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
		let mut pair =
			[unsafe { abi::pon_const_int(i64::from(entry.if_index)) }, alloc_str_object(&name)];
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
	std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO)
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	if crate::tag::is_small_int(object) {
		return Ok(crate::tag::untag_small_int(object));
	}
	match unsafe { crate::types::int::to_bigint_including_bool(object) } {
		Some(value) => value.to_i64().ok_or_else(|| {
			raise_overflow_error(&format!("{what} is too large to fit in a C integer"))
		}),
		None => Err(raise_type_error(&format!("{what} must be an integer"))),
	}
}

fn single_nonnegative_usize(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<usize, *mut PyObject> {
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
