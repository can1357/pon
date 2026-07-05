//! Native `_statistics` helpers.
//!
//! CPython exposes a tiny C accelerator for `statistics.NormalDist.inv_cdf`.
//! This is the same AS241 rational approximation used by
//! `Lib/statistics.py`, with CPython's argument conversion and domain error for
//! probabilities outside `[0, 1]`.

use std::ptr;

use num_traits::ToPrimitive;

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abstract_op,
	intern::intern,
	object::PyObject,
	types::{bool_, dict, exc::ExceptionKind, float, int},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("_statistics", [
		(intern("__name__"), str_object("_statistics")?),
		function_attr("_normal_dist_inv_cdf", normal_dist_inv_cdf_entry)?,
	])
}

unsafe extern "C" fn normal_dist_inv_cdf_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 3 || argv.is_null() {
		return raise_type_error("_normal_dist_inv_cdf() takes exactly 3 arguments");
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let p = match unsafe { coerce_f64(crate::tag::untag_arg(args[0])) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mu = match unsafe { coerce_f64(crate::tag::untag_arg(args[1])) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let sigma = match unsafe { coerce_f64(crate::tag::untag_arg(args[2])) } {
		Ok(value) => value,
		Err(error) => return error,
	};

	if p <= 0.0 || p >= 1.0 {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			"inv_cdf undefined for these parameters",
		);
	}

	float::from_f64(normal_dist_inv_cdf(p, mu, sigma))
}

fn normal_dist_inv_cdf(p: f64, mu: f64, sigma: f64) -> f64 {
	let q = p - 0.5;
	if q.abs() <= 0.425 {
		let r = 0.180625 - q * q;
		let num = horner(r, &[
			2.50908_09287_30122_6727e3,
			3.34305_75583_58812_8105e4,
			6.72657_70927_00870_0853e4,
			4.59219_53931_54987_1457e4,
			1.37316_93765_50946_1125e4,
			1.97159_09503_06551_4427e3,
			1.33141_66789_17843_7745e2,
			3.38713_28727_96366_6080e0,
		]) * q;
		let den = horner(r, &[
			5.22649_52788_52854_5610e3,
			2.87290_85735_72194_2674e4,
			3.93078_95800_09271_0610e4,
			2.12137_94301_58659_5867e4,
			5.39419_60214_24751_1077e3,
			6.87187_00749_20579_0830e2,
			4.23133_30701_60091_1252e1,
			1.0,
		]);
		return mu + (num / den * sigma);
	}

	let mut r = if q <= 0.0 { p } else { 1.0 - p };
	r = (-r.ln()).sqrt();
	let (num, den) = if r <= 5.0 {
		let r = r - 1.6;
		(
			horner(r, &[
				7.74545_01427_83414_07640e-4,
				2.27238_44989_26918_45833e-2,
				2.41780_72517_74506_11770e-1,
				1.27045_82524_52368_38258e0,
				3.64784_83247_63204_60504e0,
				5.76949_72214_60691_40550e0,
				4.63033_78461_56545_29590e0,
				1.42343_71107_49683_57734e0,
			]),
			horner(r, &[
				1.05075_00716_44416_84324e-9,
				5.47593_80849_95344_94600e-4,
				1.51986_66563_61645_71966e-2,
				1.48103_97642_74800_74590e-1,
				6.89767_33498_51000_04550e-1,
				1.67638_48301_83803_84940e0,
				2.05319_16266_37758_82187e0,
				1.0,
			]),
		)
	} else {
		let r = r - 5.0;
		(
			horner(r, &[
				2.01033_43992_92288_13265e-7,
				2.71155_55687_43487_57815e-5,
				1.24266_09473_88078_43860e-3,
				2.65321_89526_57612_30930e-2,
				2.96560_57182_85048_91230e-1,
				1.78482_65399_17291_33580e0,
				5.46378_49111_64114_36990e0,
				6.65790_46435_01103_77720e0,
			]),
			horner(r, &[
				2.04426_31033_89939_78564e-15,
				1.42151_17583_16445_88870e-7,
				1.84631_83175_10054_68180e-5,
				7.86869_13114_56132_59100e-4,
				1.48753_61290_85061_48525e-2,
				1.36929_88092_27358_05310e-1,
				5.99832_20655_58879_37690e-1,
				1.0,
			]),
		)
	};

	let mut x = num / den;
	if q < 0.0 {
		x = -x;
	}
	mu + (x * sigma)
}

fn horner(x: f64, coeffs: &[f64]) -> f64 {
	coeffs
		.iter()
		.copied()
		.fold(0.0, |acc, coefficient| acc * x + coefficient)
}

unsafe fn coerce_f64(object: *mut PyObject) -> Result<f64, *mut PyObject> {
	if let Some(value) = unsafe { float::to_f64(object) } {
		return Ok(value);
	}
	if let Some(value) = unsafe { bool_::to_bool(object) } {
		return Ok(if value { 1.0 } else { 0.0 });
	}
	if let Some(value) = unsafe { int::to_bigint(object) } {
		let converted = value.to_f64().unwrap_or(f64::INFINITY);
		if converted.is_infinite() {
			return Err(crate::abi::exc::raise_kind_error_text(
				ExceptionKind::OverflowError,
				"int too large to convert to float",
			));
		}
		return Ok(converted);
	}
	if let Some(method) = unsafe { try_get_attr(object, "__float__") } {
		let result =
			crate::tag::untag_arg(unsafe { crate::abi::pon_call(method, ptr::null_mut(), 0) });
		if result.is_null() {
			return Err(ptr::null_mut());
		}
		if let Some(value) = unsafe { float::to_f64(result) } {
			return Ok(value);
		}
		return Err(raise_type_error(&format!(
			"{}.__float__ returned non-float (type {})",
			type_name(object),
			type_name(result)
		)));
	}
	Err(raise_type_error(&format!("must be real number, not {}", type_name(object))))
}

unsafe fn try_get_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
	let result = unsafe { abstract_op::get_attr(object, intern(name)) };
	if result.is_null() {
		crate::thread_state::pon_err_clear();
		None
	} else {
		Some(crate::tag::untag_arg(result))
	}
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate _statistics.{name}"))
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
	let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
	(!object.is_null())
		.then_some(object)
		.ok_or_else(|| format!("failed to allocate string {text:?}"))
}

fn type_name(object: *mut PyObject) -> &'static str {
	unsafe { dict::type_name(object) }.unwrap_or("object")
}

fn raise_type_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}
