import os, sys
print(type(os.environ).__name__)
e = os.environ.copy()
print(type(e).__name__, 'PATH' in e)
print(sys.argv.copy() == sys.argv)
d = {'a':1}; print(d.copy())
class C: pass
