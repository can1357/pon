path = "/tmp/pon_buildreq_probe/.pon/packages/site-packages/Cython/Compiler/Builtin.py"
src = open(path).read()
src = src.replace("\ninit_builtins()\n", "\n# init_builtins() skipped by probe\n")
ns = {
    "__name__": "Cython.Compiler.Builtin_probe",
    "__package__": "Cython.Compiler",
    "__file__": path,
}
exec(compile(src, path, "exec"), ns)
orig_attr = ns["BuiltinAttribute"].declare_in_type
def attr(self, self_type):
    print("ATTR", getattr(self_type, "name", "?"), self.py_name, self.field_type_name)
    return orig_attr(self, self_type)
ns["BuiltinAttribute"].declare_in_type = attr
orig_method = ns["BuiltinMethod"].declare_in_type
def method(self, self_type):
    print("METHOD", getattr(self_type, "name", "?"), self.py_name, self.args, self.ret_type, self.builtin_return_type)
    return orig_method(self, self_type)
ns["BuiltinMethod"].declare_in_type = method
orig_prop = ns["BuiltinProperty"].declare_in_type
def prop(self, self_type):
    print("PROP", getattr(self_type, "name", "?"), self.py_name)
    return orig_prop(self, self_type)
ns["BuiltinProperty"].declare_in_type = prop
print("STEP init_builtin_structs")
ns["init_builtin_structs"]()
print("STEP init_builtin_types")
ns["init_builtin_types"]()
print("DONE")
