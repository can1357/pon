//! Native `posix` module: the OS-facing half of CPython's `os`.
//!
//! On POSIX hosts CPython's `os.py` is a thin re-export of the C `posix`
//! module (`from posix import *`), and several stdlib modules import `posix`
//! directly (`shutil`, `importlib._bootstrap_external`, `pathlib._os`). pon's
//! curated `os` already serves that surface natively, so `posix` is the same
//! attr set installed under the other name.
//!
//! Divergence note: CPython's `posix.environ` is bytes-keyed; pon serves the
//! str-keyed snapshot shared with `os.environ` (see `os::environ_snapshot`).

use super::install_module;
use crate::object::PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("posix", super::os::build_attrs("posix")?)
}
