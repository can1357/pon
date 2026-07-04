import _ctypes
print('module_ok', _ctypes.__name__)
for name in ['Array','Structure','Union','CField','CTYPES_MAX_ARGCOUNT','dlsym','dlclose','resize','buffer_info','c_char_p','c_wchar_p','py_object','c_float','c_double']:
    print('has', name, hasattr(_ctypes, name))
import ctypes
print('ctypes_ok', ctypes.__name__)
for name in ['c_char','c_wchar','c_wchar_p','py_object','create_string_buffer','cast','string_at','CFUNCTYPE']:
    print('ctypes_has', name, hasattr(ctypes, name))
x = ctypes.c_int(7)
p = ctypes.pointer(x)
print('pointer', p.contents.value, ctypes.addressof(x) != 0, ctypes.sizeof(x), ctypes.alignment(x))
print('char', ctypes.c_char(b'A').value)
print('wchar', ctypes.c_wchar('Ω').value)
print('charp', ctypes.c_char_p(b'hello').value)
print('wcharp', ctypes.c_wchar_p('hi').value)
print('pyobj', ctypes.py_object('ok').value)
print('buffer_info', _ctypes.buffer_info(ctypes.c_int))
lib = ctypes.CDLL(None)
strlen = lib.strlen
strlen.argtypes = [ctypes.c_char_p]
strlen.restype = ctypes.c_size_t
print('strlen', strlen(b'hello'))
print('string_at', ctypes.string_at(ctypes.c_char_p(b'world'), 5))
