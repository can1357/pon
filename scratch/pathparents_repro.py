import sys
import types

import ppkg.a as original

wrapper = types.ModuleType('mesonbuild._pathlib')
wrapper.PurePath = original.PurePath
sys.modules['ppkg.a'] = wrapper

import ppkg.a
print('sys-modules-binding', sys.modules['ppkg.a'].__name__)
print('property-module', original.PurePath.parents.fget.__module__)
print('parents', original.PurePath('kept').parents)
