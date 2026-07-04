import re
def show(label, fn):
    try: print(label, "=>", fn())
    except Exception as e: print(label, "ERR", type(e).__name__, e)

f = re.compile('x').match
show("callable(re.match bound)", lambda: callable(f))
show("call re.match bound", lambda: bool(f('x')))
excluded = re.compile('abc').match
show("stored excluded()", lambda: bool(excluded('abcd')))
g = {}.get
show("callable(dict.get bound)", lambda: callable(g))
show("call dict.get bound", lambda: g('k'))
# list of stored bound methods (mesonpy _compile_patterns shape)
pats = [re.compile(p).match for p in ('a', 'b')]
show("list of bound match", lambda: [bool(m('a')) for m in pats])
