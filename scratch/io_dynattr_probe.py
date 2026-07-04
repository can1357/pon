import gc
import sys

DATA = "/work/pon/tmp/io_dynattr_probe_data.txt"

with open(DATA, "w+", encoding="utf-8") as f:
    print("file mode", f.mode == "w+")
    print("file name", isinstance(f.name, str) and f.name.endswith("io_dynattr_probe_data.txt"))
    f.custom_attr = "one"
    print("file attr", f.custom_attr)
    f.custom_attr = "two"
    print("file overwrite", f.custom_attr)
    f.gc_attr = (lambda: "file-kept")
    gc.collect()
    print("file gc", f.gc_attr())
    del f.gc_attr
    print("file missing", getattr(f, "missing_attr", "fallback"))
    del f.custom_attr
    try:
        f.custom_attr
    except AttributeError as exc:
        print("file del type", type(exc).__name__)
    print("file hasattr", hasattr(f, "custom_attr"))
    f.write("abc")
    f.seek(0)
    print("file roundtrip", f.read())

for label, stream in (("stdout", sys.stdout), ("stderr", sys.stderr)):
    setattr(stream, "probe_attr", label + "-one")
    print(label, "attr", getattr(stream, "probe_attr"))
    setattr(stream, "probe_attr", label + "-two")
    print(label, "overwrite", getattr(stream, "probe_attr"))
    setattr(stream, "gc_probe_attr", (lambda value=label: value + "-kept"))
    gc.collect()
    print(label, "gc", getattr(stream, "gc_probe_attr")())
    delattr(stream, "gc_probe_attr")
    print(label, "missing", getattr(stream, "missing_probe_attr", "fallback"))
    delattr(stream, "probe_attr")
    try:
        getattr(stream, "probe_attr")
    except AttributeError as exc:
        print(label, "del type", type(exc).__name__)
    print(label, "hasattr", hasattr(stream, "probe_attr"))
