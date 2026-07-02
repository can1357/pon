# sysconfig build-time config surface: type/shape assertions only.
# CPython GENERATES _sysconfigdata_{abiflags}_{platform}_{multiarch}.py at
# build time (it is absent from any Lib/ checkout); pon serves the same
# surface from a curated native module. The VALUES are build-specific on
# both sides by construction, so a differential corpus can only pin shapes:
# get_config_var returns a str for CFLAGS, an int for Py_GIL_DISABLED, and
# None for keys absent from a build's Makefile. pon's concrete values are
# pinned by pon-runtime/src/native/sysconfigdata.rs unit tests and the
# pon-cli run_cli.rs E2E test.
import sysconfig

print(type(sysconfig.get_config_var('CFLAGS')).__name__)
print(type(sysconfig.get_config_var('Py_GIL_DISABLED')).__name__)
print(sysconfig.get_config_var('no_such_config_var_probe'))
print(type(sysconfig.get_config_vars()).__name__)
print(sysconfig._get_sysconfigdata_name().startswith('_sysconfigdata_'))
print(type(sysconfig.get_config_var('prefix')).__name__)
