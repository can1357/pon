import sysconfig
cflags = sysconfig.get_config_var('CFLAGS')
print(type(cflags).__name__)
print(repr(cflags))
print(repr(sysconfig.get_config_var('Py_GIL_DISABLED')))
print(repr(sysconfig.get_config_var('TEST_MODULES')))
print(repr(sysconfig.get_config_var('nonexistent_key_probe')))
print(sysconfig._get_sysconfigdata_name())
import _sysconfigdata__darwin_ as raw
print(type(raw.build_time_vars).__name__)
print(raw.build_time_vars['prefix'] == '')
