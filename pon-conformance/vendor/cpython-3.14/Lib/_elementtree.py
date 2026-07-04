"""Pure-Python _elementtree compatibility layer for Pon.

CPython's `_elementtree` is a C accelerator.  Pon exposes the same import surface
by forwarding to the vendored `xml.etree.ElementTree` implementation, which is
the authoritative stdlib fallback when the accelerator is absent.
"""

_factories = (None, None)

def _load():
    from xml.etree import ElementTree as ET
    return ET

def _set_factories(comment_factory, pi_factory):
    global _factories
    _factories = (comment_factory, pi_factory)
    return None

def __getattr__(name):
    if name == "_set_factories":
        return _set_factories
    ET = _load()
    try:
        return getattr(ET, name)
    except AttributeError:
        raise AttributeError("module '_elementtree' has no attribute %r" % (name,)) from None

try:
    _ET = _load()
except Exception:
    _ET = None

if _ET is not None:
    for _name in ("Element", "SubElement", "ParseError", "TreeBuilder", "XMLParser"):
        if hasattr(_ET, _name):
            globals()[_name] = getattr(_ET, _name)

__all__ = ["Element", "SubElement", "ParseError", "TreeBuilder", "XMLParser"]
