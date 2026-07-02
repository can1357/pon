# Derived from CPython v3.14.0 Lib/test/test_string_literals.py topics (PSF license).

normal_hex = "\x41"
raw_hex = r"\x41"
normal_unicode = "\u00ff"
raw_unicode = r"\u00ff"
astral = "\U0001d120"
raw_astral = r"\U0001d120"
line_escape = "first\nsecond"
raw_line = r"first\nsecond"
tab_escape = "left\tright"
raw_tab = r"left\tright"

print(normal_hex, normal_hex == "A")
print(len(normal_hex), len(raw_hex), raw_hex == "\\x41")
print(normal_unicode == "\xff")
print(len(raw_unicode), raw_unicode == "\\u00ff")
print(astral == "𝄠")
print(len(raw_astral), raw_astral == "\\U0001d120")

print(len(line_escape.split("\n")))
print(line_escape.startswith("first"))
print(line_escape.endswith("second"))
print(len(raw_line), raw_line.endswith("second"))

print(len(tab_escape.split()))
print(tab_escape.startswith("left"))
print(len(raw_tab), raw_tab.find("\\t"))

triple = """alpha
beta
gamma"""
print(len(triple.split("\n")))
print(triple.startswith("alpha"))
print(triple.endswith("gamma"))
