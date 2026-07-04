from __future__ import annotations
import annotationlib

class C:
    x: undefined_name.Path | None

print("has __annotate__:", hasattr(C, "__annotate__"), C.__annotate__)
for name, fmt in [("VALUE",1),("VALUE_WITH_FAKE_GLOBALS",2),("FORWARDREF",3),("STRING",4)]:
    try:
        print(name, "=>", C.__annotate__(fmt))
    except Exception as e:
        print(name, "ERR", type(e).__name__, e)
print("get_annotations FORWARDREF:", end=" ")
try:
    print(annotationlib.get_annotations(C, format=annotationlib.Format.FORWARDREF))
except Exception as e:
    print("ERR", type(e).__name__, e)
