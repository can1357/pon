//! `_asyncio` compatibility backed by the vendored pure-Python asyncio
//! implementation.
//!
//! CPython's module is a C accelerator for objects and task bookkeeping that
//! already have authoritative Python implementations in `Lib/asyncio`. Pon has
//! no separate event-loop/task runtime in Rust, so the native row deliberately
//! stays out of asyncio's own bootstrap imports (raising `ImportError` there so
//! the stdlib fallbacks bind), then re-exports those fallback objects for
//! direct `_asyncio` imports.

use std::sync::atomic::{AtomicBool, Ordering};

use super::install_module;
use crate::{
	intern::{intern, resolve},
	object::PyObject,
	thread_state::{pon_err_clear, pon_err_message},
};

static BUILDING: AtomicBool = AtomicBool::new(false);

const EXPORTS: [(&str, &str); 16] = [
	("asyncio.events", "_get_running_loop"),
	("asyncio.events", "_set_running_loop"),
	("asyncio.events", "get_event_loop"),
	("asyncio.events", "get_running_loop"),
	("asyncio.futures", "Future"),
	("asyncio.futures", "future_add_to_awaited_by"),
	("asyncio.futures", "future_discard_from_awaited_by"),
	("asyncio.tasks", "Task"),
	("asyncio.tasks", "_enter_task"),
	("asyncio.tasks", "_leave_task"),
	("asyncio.tasks", "_register_eager_task"),
	("asyncio.tasks", "_register_task"),
	("asyncio.tasks", "_swap_current_task"),
	("asyncio.tasks", "_unregister_eager_task"),
	("asyncio.tasks", "_unregister_task"),
	("asyncio.tasks", "all_tasks"),
];

const EXTRA_EXPORTS: [(&str, &str); 1] = [("asyncio.tasks", "current_task")];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	if active_asyncio_import() || BUILDING.swap(true, Ordering::AcqRel) {
		return Err("No module named '_asyncio'".to_owned());
	}

	let result = build_module();
	BUILDING.store(false, Ordering::Release);
	result
}

fn build_module() -> Result<*mut PyObject, String> {
	for module_name in ["asyncio.events", "asyncio.futures", "asyncio.tasks"] {
		import_exact(module_name)?;
	}

	let mut attrs = Vec::with_capacity(1 + EXPORTS.len() + EXTRA_EXPORTS.len());
	attrs.push((intern("__name__"), str_object("_asyncio")?));
	for (module_name, attr_name) in EXPORTS.into_iter().chain(EXTRA_EXPORTS) {
		let value = crate::import::module_attr(intern(module_name), intern(attr_name))
			.ok_or_else(|| format!("{module_name}.{attr_name} is unavailable for _asyncio"))?;
		attrs.push((intern(attr_name), value));
	}
	install_module("_asyncio", attrs)
}

fn active_asyncio_import() -> bool {
	crate::import::active_module_name_id()
		.and_then(resolve)
		.is_some_and(|name| name == "asyncio" || name.starts_with("asyncio."))
}

fn import_exact(name: &str) -> Result<*mut PyObject, String> {
	let module = crate::import::import_named_module_raw(name);
	if module.is_null() {
		let message = pon_err_message().unwrap_or_else(|| format!("failed to import {name}"));
		pon_err_clear();
		Err(message)
	} else {
		Ok(module)
	}
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
	let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
	(!object.is_null())
		.then_some(object)
		.ok_or_else(|| format!("failed to allocate string {text:?}"))
}
