def show(label, fn):
    try:
        print(label, "=>", fn())
    except Exception as e:
        print(label, "ERR", type(e).__name__, repr(str(e)))

import packaging.specifiers as S
show("SpecifierSet >=1.0", lambda: str(S.SpecifierSet(">=1.0")))
show("Specifier >=1.0", lambda: str(S.Specifier(">=1.0")))

import packaging._tokenizer as T
import packaging._parser as P
def tok():
    tk = T.Tokenizer("numpy>=1.0", rules=T.DEFAULT_RULES)
    out = []
    for _ in range(12):
        m = tk.match
        # emulate: peek then read
        break
    return "constructed"
show("Tokenizer construct", tok)

def parse_named():
    return P.parse_requirement("numpy>=1.0")
show("parse_requirement numpy>=1.0", parse_named)
show("parse_requirement numpy", lambda: P.parse_requirement("numpy"))
