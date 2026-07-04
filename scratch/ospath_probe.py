import os, os.path
for name in ["walk","makedirs","path"]:
    print("os."+name, "callable" if callable(getattr(os, name, None)) else ("MISSING" if not hasattr(os,name) else "not-callable"))
for name in ["isfile","abspath","normpath","isdir","exists","join","dirname","basename","relpath","realpath","splitext"]:
    v = getattr(os.path, name, None)
    print("os.path."+name, "callable" if callable(v) else ("MISSING" if not hasattr(os.path,name) else "not-callable"))
