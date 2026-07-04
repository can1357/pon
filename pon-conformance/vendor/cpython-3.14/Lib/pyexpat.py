"""Small Expat-compatible XML parser for Pon's stdlib bootstrap.

This is a real event parser for well-formed XML documents used by plistlib and
ElementTree's pure-Python fallback.  It implements pyexpat's callback surface in
Python rather than pretending the C extension exists.
"""

import re
import sys
import types

version_info = (2, 6, 2)
EXPAT_VERSION = "expat_2.6.2-pon"
native_encoding = "UTF-8"

XML_PARAM_ENTITY_PARSING_NEVER = 0
XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE = 1
XML_PARAM_ENTITY_PARSING_ALWAYS = 2

# Expat exposes this informational feature table at module scope.  Pon's
# Python parser is UTF-8/Unicode backed, has a finite context window, supports
# namespace callbacks, and deliberately does not advertise the C-only DTD or
# general-entity engines.
features = [
    ("sizeof(XML_Char)", 1),
    ("sizeof(XML_LChar)", 1),
    ("XML_DTD", 0),
    ("XML_CONTEXT_BYTES", 1024),
    ("XML_NS", 1),
    ("XML_BLAP_MAX_AMP", 100),
    ("XML_BLAP_ACT_THRES", 8388608),
    ("XML_GE", 0),
    ("XML_AT_MAX_AMP", 100),
    ("XML_AT_ACT_THRES", 67108864),
]

XML_ERROR_NONE = 0
XML_ERROR_NO_MEMORY = 1
XML_ERROR_SYNTAX = 2
XML_ERROR_NO_ELEMENTS = 3
XML_ERROR_INVALID_TOKEN = 4
XML_ERROR_UNCLOSED_TOKEN = 5
XML_ERROR_PARTIAL_CHAR = 6
XML_ERROR_TAG_MISMATCH = 7
XML_ERROR_DUPLICATE_ATTRIBUTE = 8
XML_ERROR_JUNK_AFTER_DOC_ELEMENT = 9
XML_ERROR_PARAM_ENTITY_REF = 10
XML_ERROR_UNDEFINED_ENTITY = 11

class ExpatError(Exception):
    def __init__(self, message, code=XML_ERROR_SYNTAX, lineno=1, offset=0):
        super().__init__(message)
        self.code = code
        self.lineno = lineno
        self.offset = offset

error = ExpatError

_MESSAGES = {
    XML_ERROR_NONE: "no error",
    XML_ERROR_SYNTAX: "syntax error",
    XML_ERROR_NO_ELEMENTS: "no element found",
    XML_ERROR_INVALID_TOKEN: "not well-formed (invalid token)",
    XML_ERROR_UNCLOSED_TOKEN: "unclosed token",
    XML_ERROR_TAG_MISMATCH: "mismatched tag",
    XML_ERROR_DUPLICATE_ATTRIBUTE: "duplicate attribute",
    XML_ERROR_JUNK_AFTER_DOC_ELEMENT: "junk after document element",
    XML_ERROR_UNDEFINED_ENTITY: "undefined entity",
}

def ErrorString(code):
    return _MESSAGES.get(code, "unknown error")

errors = types.ModuleType("pyexpat.errors")
for _name, _value in list(globals().items()):
    if _name.startswith("XML_ERROR_"):
        setattr(errors, _name, _value)
        setattr(errors, _name[10:].lower(), ErrorString(_value))
errors.messages = _MESSAGES

model = types.ModuleType("pyexpat.model")
model.XML_CTYPE_EMPTY = 1
model.XML_CTYPE_ANY = 2
model.XML_CTYPE_MIXED = 3
model.XML_CTYPE_NAME = 4
model.XML_CTYPE_CHOICE = 5
model.XML_CTYPE_SEQ = 6
model.XML_CQUANT_NONE = 0
model.XML_CQUANT_OPT = 1
model.XML_CQUANT_REP = 2
model.XML_CQUANT_PLUS = 3

sys.modules.setdefault("pyexpat.errors", errors)
sys.modules.setdefault("pyexpat.model", model)

_NAME_RE = re.compile(r"[A-Za-z_:\u0080-\uffff][A-Za-z0-9_.:\-\u0080-\uffff]*")
_ENTITY_RE = re.compile(r"<!ENTITY\s+(%\s+)?([A-Za-z_][\w.:-]*)\s+(['\"])(.*?)\3", re.S)

class xmlparser:
    def __init__(self, encoding=None, namespace_separator=None, intern=None):
        self.encoding = encoding or "utf-8"
        self.namespace_separator = namespace_separator
        self.intern = intern
        self.buffer_text = False
        self.buffer_size = 8192
        self.ordered_attributes = False
        self.specified_attributes = False
        self.returns_unicode = True
        self.StartElementHandler = None
        self.EndElementHandler = None
        self.CharacterDataHandler = None
        self.ProcessingInstructionHandler = None
        self.CommentHandler = None
        self.StartNamespaceDeclHandler = None
        self.EndNamespaceDeclHandler = None
        self.DefaultHandler = None
        self.DefaultHandlerExpand = None
        self.EntityDeclHandler = None
        self.StartDoctypeDeclHandler = None
        self.EndDoctypeDeclHandler = None
        self.ExternalEntityRefHandler = None
        self.CurrentLineNumber = 1
        self.CurrentColumnNumber = 0
        self.ErrorLineNumber = 1
        self.ErrorColumnNumber = 0
        self.ErrorCode = XML_ERROR_NONE
        self._buffer = ""
        self._closed = False
        self._base = None
        self._reparse_deferral = True
        self._ns_stack = []
        self._ns = {"xml": "http://www.w3.org/XML/1998/namespace",
                    "xmlns": "http://www.w3.org/2000/xmlns/"}
        self._element_stack = []
        self._seen_root = False
        self._finished_root = False

    def Parse(self, data, isfinal=False):
        if isinstance(data, str):
            text = data
        else:
            text = bytes(data).decode(self.encoding or "utf-8")
        self._buffer += text
        try:
            self._parse_buffer(bool(isfinal))
        except ExpatError as exc:
            self.ErrorCode = exc.code
            self.ErrorLineNumber = exc.lineno
            self.ErrorColumnNumber = exc.offset
            raise
        if isfinal:
            if self._element_stack:
                self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
            if not self._seen_root:
                self._raise(XML_ERROR_NO_ELEMENTS, "no element found")
            self._closed = True
        return 1

    def ParseFile(self, file):
        chunks = []
        while True:
            chunk = file.read(65536)
            if not chunk:
                break
            chunks.append(chunk)
        data = b"".join(chunks) if chunks and not isinstance(chunks[0], str) else "".join(chunks)
        return self.Parse(data, True)

    def SetBase(self, base):
        self._base = base

    def GetBase(self):
        return self._base

    def UseForeignDTD(self, flag=True):
        return None

    def SetParamEntityParsing(self, flag):
        return 1

    def ExternalEntityParserCreate(self, context, encoding=None):
        return xmlparser(encoding or self.encoding, self.namespace_separator, self.intern)

    def SetReparseDeferralEnabled(self, enabled):
        self._reparse_deferral = bool(enabled)

    def GetReparseDeferralEnabled(self):
        return self._reparse_deferral

    def _parse_buffer(self, isfinal):
        s = self._buffer
        pos = 0
        while pos < len(s):
            lt = s.find("<", pos)
            if lt < 0:
                if isfinal:
                    self._data(s[pos:])
                    pos = len(s)
                break
            if lt > pos:
                self._data(s[pos:lt])
                pos = lt
            if s.startswith("<!--", pos):
                end = s.find("-->", pos + 4)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                text = s[pos+4:end]
                if self.CommentHandler: self.CommentHandler(text)
                self._default(s[pos:end+3])
                self._advance(s[pos:end+3])
                pos = end + 3
            elif s.startswith("<![CDATA[", pos):
                end = s.find("]]>", pos + 9)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                self._data(s[pos+9:end], raw=True)
                self._advance(s[pos:end+3])
                pos = end + 3
            elif s.startswith("<?", pos):
                end = s.find("?>", pos + 2)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                body = s[pos+2:end].strip()
                if body and self.ProcessingInstructionHandler:
                    parts = body.split(None, 1)
                    self.ProcessingInstructionHandler(parts[0], parts[1] if len(parts) > 1 else "")
                self._advance(s[pos:end+2])
                pos = end + 2
            elif s.startswith("<!DOCTYPE", pos):
                end = self._find_doctype_end(s, pos)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                decl = s[pos:end+1]
                self._doctype(decl)
                self._advance(decl)
                pos = end + 1
            elif s.startswith("</", pos):
                end = s.find(">", pos + 2)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                name = s[pos+2:end].strip()
                self._end(name)
                self._advance(s[pos:end+1])
                pos = end + 1
            else:
                end = self._find_tag_end(s, pos)
                if end < 0:
                    if isfinal: self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
                    break
                body = s[pos+1:end]
                empty = body.rstrip().endswith("/")
                if empty:
                    body = body.rstrip()[:-1]
                self._start(body.strip(), empty)
                self._advance(s[pos:end+1])
                pos = end + 1
        self._buffer = s[pos:]

    def _start(self, body, empty):
        match = _NAME_RE.match(body)
        if not match:
            self._raise(XML_ERROR_INVALID_TOKEN, "not well-formed (invalid token)")
        raw_name = match.group(0)
        attrs = self._attrs(body[match.end():])
        declared = []
        normal_attrs = []
        for key, value in attrs:
            if key == "xmlns":
                declared.append(("", value))
            elif key.startswith("xmlns:"):
                declared.append((key[6:], value))
            else:
                normal_attrs.append((key, value))
        for prefix, uri in declared:
            self._ns[prefix] = uri
            if self.StartNamespaceDeclHandler:
                self.StartNamespaceDeclHandler(prefix or None, uri)
        self._ns_stack.append(declared)
        name = self._expand_name(raw_name, is_attr=False)
        expanded = [(self._expand_name(k, is_attr=True), v) for k, v in normal_attrs]
        if self.ordered_attributes:
            attr_arg = []
            for key, value in expanded:
                attr_arg.extend([key, value])
        else:
            attr_arg = dict(expanded)
        if self._finished_root:
            self._raise(XML_ERROR_JUNK_AFTER_DOC_ELEMENT, "junk after document element")
        self._seen_root = True
        self._element_stack.append(name)
        if self.StartElementHandler:
            self.StartElementHandler(name, attr_arg)
        if empty:
            self._end(raw_name)

    def _end(self, raw_name):
        name = self._expand_name(raw_name, is_attr=False)
        if not self._element_stack or self._element_stack[-1] != name:
            self._raise(XML_ERROR_TAG_MISMATCH, "mismatched tag")
        self._element_stack.pop()
        if self.EndElementHandler:
            self.EndElementHandler(name)
        declared = self._ns_stack.pop()
        for prefix, _uri in reversed(declared):
            if self.EndNamespaceDeclHandler:
                self.EndNamespaceDeclHandler(prefix or None)
            # Rebuild current binding for this prefix from outer stack.
            self._ns.pop(prefix, None)
            for scope in self._ns_stack:
                for p, u in scope:
                    if p == prefix:
                        self._ns[prefix] = u
        if not self._element_stack:
            self._finished_root = True

    def _attrs(self, text):
        attrs = []
        seen = set()
        i = 0
        n = len(text)
        while i < n:
            while i < n and text[i].isspace():
                i += 1
            if i >= n:
                break
            match = _NAME_RE.match(text, i)
            if not match:
                self._raise(XML_ERROR_INVALID_TOKEN, "not well-formed (invalid token)")
            key = match.group(0)
            i = match.end()
            while i < n and text[i].isspace():
                i += 1
            if i >= n or text[i] != "=":
                self._raise(XML_ERROR_INVALID_TOKEN, "not well-formed (invalid token)")
            i += 1
            while i < n and text[i].isspace():
                i += 1
            if i >= n or text[i] not in "'\"":
                self._raise(XML_ERROR_INVALID_TOKEN, "not well-formed (invalid token)")
            quote = text[i]
            i += 1
            j = text.find(quote, i)
            if j < 0:
                self._raise(XML_ERROR_UNCLOSED_TOKEN, "unclosed token")
            if key in seen:
                self._raise(XML_ERROR_DUPLICATE_ATTRIBUTE, "duplicate attribute")
            seen.add(key)
            attrs.append((key, self._unescape(text[i:j])))
            i = j + 1
        return attrs

    def _doctype(self, decl):
        self._default(decl)
        parts = decl[9:].strip().strip(">").split()
        if parts and self.StartDoctypeDeclHandler:
            self.StartDoctypeDeclHandler(parts[0], None, None, 0)
        for match in _ENTITY_RE.finditer(decl):
            if self.EntityDeclHandler:
                is_param = 1 if match.group(1) else 0
                self.EntityDeclHandler(match.group(2), is_param, match.group(4), None, None, None, None)
        if self.EndDoctypeDeclHandler:
            self.EndDoctypeDeclHandler()

    def _data(self, text, raw=False):
        if not text:
            return
        if not self._element_stack:
            if text.strip():
                self._raise(XML_ERROR_JUNK_AFTER_DOC_ELEMENT, "junk after document element")
            self._advance(text)
            return
        data = text if raw else self._unescape(text)
        if data and self.CharacterDataHandler:
            self.CharacterDataHandler(data)
        self._advance(text)

    def _default(self, text):
        handler = self.DefaultHandlerExpand or self.DefaultHandler
        if handler:
            handler(text)

    def _unescape(self, text):
        def repl(match):
            name = match.group(1)
            if name == "lt": return "<"
            if name == "gt": return ">"
            if name == "amp": return "&"
            if name == "apos": return "'"
            if name == "quot": return '"'
            if name.startswith("#x"):
                return chr(int(name[2:], 16))
            if name.startswith("#"):
                return chr(int(name[1:], 10))
            self._default("&" + name + ";")
            self._raise(XML_ERROR_UNDEFINED_ENTITY, "undefined entity")
        return re.sub(r"&([^;]+);", repl, text)

    def _expand_name(self, name, is_attr):
        if self.namespace_separator is None:
            return name
        if ":" in name:
            prefix, local = name.split(":", 1)
            uri = self._ns.get(prefix)
            return (uri + self.namespace_separator + local) if uri else name
        if not is_attr and "" in self._ns:
            return self._ns[""] + self.namespace_separator + name
        return name

    def _find_tag_end(self, s, pos):
        quote = None
        i = pos + 1
        while i < len(s):
            ch = s[i]
            if quote:
                if ch == quote:
                    quote = None
            elif ch in "'\"":
                quote = ch
            elif ch == ">":
                return i
            i += 1
        return -1

    def _find_doctype_end(self, s, pos):
        quote = None
        depth = 0
        i = pos + 9
        while i < len(s):
            ch = s[i]
            if quote:
                if ch == quote:
                    quote = None
            elif ch in "'\"":
                quote = ch
            elif ch == "[":
                depth += 1
            elif ch == "]" and depth:
                depth -= 1
            elif ch == ">" and depth == 0:
                return i
            i += 1
        return -1

    def _advance(self, text):
        if not text:
            return
        lines = text.split("\n")
        if len(lines) == 1:
            self.CurrentColumnNumber += len(text)
        else:
            self.CurrentLineNumber += len(lines) - 1
            self.CurrentColumnNumber = len(lines[-1])

    def _raise(self, code, message):
        raise ExpatError(message, code, self.CurrentLineNumber, self.CurrentColumnNumber)

XMLParserType = xmlparser

def ParserCreate(encoding=None, namespace_separator=None, intern=None):
    return xmlparser(encoding, namespace_separator, intern)

__all__ = [
    "EXPAT_VERSION", "ErrorString", "ExpatError", "ParserCreate", "XMLParserType",
    "XML_PARAM_ENTITY_PARSING_ALWAYS", "XML_PARAM_ENTITY_PARSING_NEVER",
    "XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE", "error", "errors", "features",
    "model", "native_encoding", "version_info", "xmlparser",
]
for _name in list(globals()):
    if _name.startswith("XML_ERROR_"):
        __all__.append(_name)
