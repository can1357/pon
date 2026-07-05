//! Native macOS `_scproxy` module backed by SystemConfiguration.
//!
//! `urllib.request` imports this helper on Darwin to read proxy and bypass
//! settings from the same framework CPython uses.

use std::{
	ffi::{CStr, CString, c_char, c_void},
	ptr,
};

use super::install_module;
use crate::{
	abi::{self, pon_const_str, pon_make_function},
	intern::intern,
	object::PyObject,
	types::{bool_, exc::ExceptionKind},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
const K_CFNUMBER_SINT32_TYPE: i32 = 3;
const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;

type CFTypeRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type Boolean = u8;
type CFIndex = isize;

const KEY_EXCLUDE_SIMPLE: &str = "ExcludeSimpleHostnames";
const KEY_EXCEPTIONS_LIST: &str = "ExceptionsList";

struct ProxySpec {
	scheme:      &'static str,
	enabled_key: &'static str,
	host_key:    &'static str,
	port_key:    &'static str,
}

const PROXY_SPECS: &[ProxySpec] = &[
	ProxySpec {
		scheme:      "http",
		enabled_key: "HTTPEnable",
		host_key:    "HTTPProxy",
		port_key:    "HTTPPort",
	},
	ProxySpec {
		scheme:      "https",
		enabled_key: "HTTPSEnable",
		host_key:    "HTTPSProxy",
		port_key:    "HTTPSPort",
	},
	ProxySpec {
		scheme:      "ftp",
		enabled_key: "FTPEnable",
		host_key:    "FTPProxy",
		port_key:    "FTPPort",
	},
	ProxySpec {
		scheme:      "gopher",
		enabled_key: "GopherEnable",
		host_key:    "GopherProxy",
		port_key:    "GopherPort",
	},
	ProxySpec {
		scheme:      "socks",
		enabled_key: "SOCKSEnable",
		host_key:    "SOCKSProxy",
		port_key:    "SOCKSPort",
	},
];

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
	fn SCDynamicStoreCopyProxies(store: CFTypeRef) -> CFDictionaryRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
	fn CFRelease(cf: CFTypeRef);
	fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
	fn CFNumberGetValue(number: CFTypeRef, number_type: i32, value: *mut c_void) -> Boolean;
	fn CFStringCreateWithCString(
		allocator: CFTypeRef,
		string: *const c_char,
		encoding: u32,
	) -> CFStringRef;
	fn CFStringGetLength(string: CFStringRef) -> CFIndex;
	fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
	fn CFStringGetCString(
		string: CFStringRef,
		buffer: *mut c_char,
		buffer_size: CFIndex,
		encoding: u32,
	) -> Boolean;
	fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
	fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> CFTypeRef;
}

struct OwnedCf(CFTypeRef);

impl OwnedCf {
	fn as_ptr(&self) -> CFTypeRef {
		self.0
	}
}

impl Drop for OwnedCf {
	fn drop(&mut self) {
		if !self.0.is_null() {
			// SAFETY: `OwnedCf` only wraps objects returned at +1 retain count.
			unsafe { CFRelease(self.0) };
		}
	}
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_scproxy";
	let name_obj = alloc_str_object(name);
	if name_obj.is_null() {
		return Err("failed to allocate _scproxy.__name__".to_owned());
	}
	let attrs = vec![
		(intern("__name__"), name_obj),
		function_attr("_get_proxy_settings", scproxy_get_proxy_settings)?,
		function_attr("_get_proxies", scproxy_get_proxies)?,
	];
	install_module(name, attrs)
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let object = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate _scproxy.{name}"))
}

unsafe extern "C" fn scproxy_get_proxy_settings(
	_argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 0 {
		return raise_type_error("_scproxy._get_proxy_settings() takes no arguments");
	}
	let Some(proxy_dict) = copy_proxies() else {
		// CPython returns None when SCDynamicStoreCopyProxies itself fails.
		return unsafe { abi::pon_none() };
	};
	build_proxy_settings(proxy_dict.as_ptr())
}

unsafe extern "C" fn scproxy_get_proxies(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return raise_type_error("_scproxy._get_proxies() takes no arguments");
	}
	let Some(proxy_dict) = copy_proxies() else {
		return empty_dict();
	};
	build_proxies(proxy_dict.as_ptr())
}

fn copy_proxies() -> Option<OwnedCf> {
	// SAFETY: NULL asks SystemConfiguration for the process-global dynamic store.
	let proxy_dict = unsafe { SCDynamicStoreCopyProxies(ptr::null()) };
	(!proxy_dict.is_null()).then_some(OwnedCf(proxy_dict))
}

fn build_proxy_settings(proxy_dict: CFDictionaryRef) -> *mut PyObject {
	let exclude_simple =
		dictionary_number(proxy_dict, KEY_EXCLUDE_SIMPLE).is_some_and(|value| value != 0);
	let mut pairs = Vec::with_capacity(4);
	if !push_pair(&mut pairs, "exclude_simple", bool_::from_bool(exclude_simple)) {
		return ptr::null_mut();
	}
	let exceptions = dictionary_value(proxy_dict, KEY_EXCEPTIONS_LIST);
	if !exceptions.is_null() {
		let tuple = build_exceptions_tuple(exceptions);
		if tuple.is_null() || !push_pair(&mut pairs, "exceptions", tuple) {
			return ptr::null_mut();
		}
	}
	build_map(pairs)
}

fn build_exceptions_tuple(array: CFArrayRef) -> *mut PyObject {
	// SAFETY: SystemConfiguration supplies a CFArray for the exceptions key.
	let count = unsafe { CFArrayGetCount(array) };
	let Ok(len) = usize::try_from(count) else {
		return ptr::null_mut();
	};
	let mut items = Vec::with_capacity(len);
	for index in 0..count {
		// SAFETY: `index` is within `0..CFArrayGetCount(array)`.
		let value = unsafe { CFArrayGetValueAtIndex(array, index) };
		if value.is_null() {
			// SAFETY: Singleton accessor.
			items.push(unsafe { abi::pon_none() });
			continue;
		}
		match cf_string_to_string(value) {
			Some(text) => {
				let object = alloc_str_object(&text);
				if object.is_null() {
					return ptr::null_mut();
				}
				items.push(object);
			},
			None => {
				// SAFETY: Singleton accessor.
				items.push(unsafe { abi::pon_none() });
			},
		}
	}
	// SAFETY: `items` holds live object slots for the whole call.  CPython's
	// `_scproxy` returns a tuple here despite urllib's test doc using a list.
	unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn build_proxies(proxy_dict: CFDictionaryRef) -> *mut PyObject {
	let mut pairs = Vec::with_capacity(PROXY_SPECS.len() * 2);
	for spec in PROXY_SPECS {
		if !proxy_enabled(proxy_dict, spec) {
			continue;
		}
		let host = dictionary_value(proxy_dict, spec.host_key);
		if host.is_null() {
			continue;
		}
		let Some(host) = cf_string_to_string(host) else {
			continue;
		};
		let value = match dictionary_number(proxy_dict, spec.port_key) {
			Some(port) => format!("http://{host}:{port}"),
			None => format!("http://{host}"),
		};
		let value = alloc_str_object(&value);
		if value.is_null() || !push_pair(&mut pairs, spec.scheme, value) {
			return ptr::null_mut();
		}
	}
	build_map(pairs)
}

fn proxy_enabled(proxy_dict: CFDictionaryRef, spec: &ProxySpec) -> bool {
	dictionary_number(proxy_dict, spec.enabled_key).is_some_and(|value| value != 0)
}

fn dictionary_number(dictionary: CFDictionaryRef, key: &str) -> Option<i32> {
	let value = dictionary_value(dictionary, key);
	if value.is_null() {
		None
	} else {
		cf_number_to_i32(value)
	}
}

fn dictionary_value(dictionary: CFDictionaryRef, key: &str) -> CFTypeRef {
	let Some(key_ref) = cf_string(key) else {
		return ptr::null();
	};
	// SAFETY: `dictionary` is alive for the call and `key_ref` is a CFString.
	unsafe { CFDictionaryGetValue(dictionary, key_ref.as_ptr()) }
}

fn cf_string(text: &str) -> Option<OwnedCf> {
	let text = CString::new(text).ok()?;
	// SAFETY: CoreFoundation copies the NUL-terminated UTF-8 key string.
	let string =
		unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), K_CFSTRING_ENCODING_UTF8) };
	(!string.is_null()).then_some(OwnedCf(string))
}

fn cf_number_to_i32(number: CFTypeRef) -> Option<i32> {
	let mut value = 0_i32;
	// SAFETY: `value` points to enough storage for `kCFNumberSInt32Type`.
	let ok =
		unsafe { CFNumberGetValue(number, K_CFNUMBER_SINT32_TYPE, (&mut value as *mut i32).cast()) };
	(ok != 0).then_some(value)
}

fn cf_string_to_string(string: CFStringRef) -> Option<String> {
	// SAFETY: `string` is a borrowed live CFStringRef.
	let length = unsafe { CFStringGetLength(string) };
	if length < 0 {
		return None;
	}
	// SAFETY: Pure size query for the requested encoding.
	let max_size = unsafe { CFStringGetMaximumSizeForEncoding(length, K_CFSTRING_ENCODING_UTF8) };
	let buffer_size = max_size.checked_add(1)?;
	let len = usize::try_from(buffer_size).ok()?;
	let mut buffer = vec![0 as c_char; len];
	// SAFETY: `buffer` has `buffer_size` bytes, including room for the NUL.
	let ok = unsafe {
		CFStringGetCString(string, buffer.as_mut_ptr(), buffer_size, K_CFSTRING_ENCODING_UTF8)
	};
	if ok == 0 {
		return None;
	}
	// SAFETY: `CFStringGetCString` wrote a NUL-terminated C string on success.
	unsafe { CStr::from_ptr(buffer.as_ptr()) }
		.to_str()
		.ok()
		.map(str::to_owned)
}

fn push_pair(flat: &mut Vec<*mut PyObject>, key: &str, value: *mut PyObject) -> bool {
	if value.is_null() {
		return false;
	}
	let key = alloc_str_object(key);
	if key.is_null() {
		return false;
	}
	flat.push(key);
	flat.push(value);
	true
}

fn build_map(mut flat: Vec<*mut PyObject>) -> *mut PyObject {
	// SAFETY: `flat` stores key/value pairs for the duration of the call.
	unsafe { abi::map::pon_build_map(flat.as_mut_ptr(), flat.len() / 2) }
}

fn empty_dict() -> *mut PyObject {
	// SAFETY: NULL with zero pairs builds an empty dict.
	unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) }
}

fn alloc_str_object(text: &str) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}
