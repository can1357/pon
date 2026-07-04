from Cython.Compiler.Symtab import StructOrUnionScope
from Cython.Compiler import PyrexTypes

scope = StructOrUnionScope('Py_buffer')
entry = scope.declare_var('buf', PyrexTypes.c_void_ptr_type, None, 'buf', allow_pyobject=True)
print(entry.name)

class RaisesStr:
    def __str__(self):
        raise ValueError('inner-str')

try:
    str(RaisesStr())
except Exception as exc:
    print(type(exc).__name__, str(exc))
