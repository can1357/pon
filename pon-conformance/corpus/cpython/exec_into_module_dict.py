# exec into a synthetic module's live __dict__ (importlib.util's
# spec.loader.exec_module shape, numpy's genapi loads conv_template this
# way): global stores must land on the module object.
import importlib.util
import os
import tempfile
import types

m = types.ModuleType('synthmod')
exec('Q = 42\ndef f():\n    return Q + 1\n', m.__dict__)
print(m.Q, m.f())
print(m.__dict__['Q'])

with tempfile.TemporaryDirectory() as tmp:
    p = os.path.join(tmp, 'target_mod.py')
    with open(p, 'w') as fh:
        fh.write('X = 5\ndef g():\n    return X * 2\nY = g()\n')
    spec = importlib.util.spec_from_file_location('target_mod', p)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    print(mod.X, mod.g(), mod.Y)
    print(sorted(k for k in dir(mod) if not k.startswith('__')))
