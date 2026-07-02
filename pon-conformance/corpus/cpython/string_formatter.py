# _string-backed string.Formatter surface: parse round-trips, vformat,
# format_map, and formatter_field_name_split traversal.
import _string
import string

fmt = string.Formatter()

# formatter_parser round-trips (literal, field, spec, conversion tuples).
for template in [
    "",
    "plain",
    "{x}",
    "{}",
    "a{{b}}c",
    "ab{0!r:>{1}}cd",
    "{x.y[0]:{w}.{p}}",
    "{:d}",
    "{!r}",
    "pre{{mid}}post{q}",
    "{0[a]}!{x.y:>{w}}",
]:
    print(repr(template), list(fmt.parse(template)))

it = fmt.parse("ok{a}bad}")
print(next(it))
try:
    print(next(it))
except ValueError as exc:
    print("parse error:", exc)

for bad in ["{", "a}b", "{x", "{x:{y}", "{x!rr}"]:
    try:
        list(fmt.parse(bad))
        print("no error??", repr(bad))
    except ValueError as exc:
        print(repr(bad), "->", exc)

# formatter_field_name_split: head int/str rule and (is_attr, name) items.
for field in ["0.name[1]", "a[b][-1].c", "x", "007", "[0]", "", "a[b.c]", "a[:]"]:
    first, rest = _string.formatter_field_name_split(field)
    print(repr(field), repr(first), list(rest))

for bad in ["a..b", "a[0", "a[]", "a]x"]:
    first, rest = _string.formatter_field_name_split(bad)
    try:
        items = list(rest)
        print("split ok?", repr(bad), items)
    except ValueError as exc:
        print(repr(bad), "->", exc)

# format / vformat / format_map end-to-end.
class P:
    def __init__(self, y):
        self.y = y

print(fmt.format("{0[a]}!{x.y:>{w}}", {"a": "A"}, x=P(7), w=5))
print(fmt.vformat("{0} and {1} and {0}", ("a", "b"), {}))
print(fmt.format("auto {} {} manual {2}".replace("{2}", "{ok}"), 1, 2, ok="end"))
print("{name} is {adj}".format_map({"name": "pon", "adj": "fast"}))
print("{0:{1}}|{0:>{1}}|{0:^{1}}".format("m", 5))
print(fmt.format("{a!r}+{b!s}", a="q", b=3))

# Manual/automatic numbering switch is a ValueError.
try:
    fmt.format("{} {1}", 1, 2)
except ValueError as exc:
    print("switch:", exc)

# Missing keys raise KeyError through vformat's get_value.
try:
    fmt.format("{missing}", present=1)
except KeyError as exc:
    print("missing:", exc)

# get_field traverses attributes and items from the split iterator.
print(fmt.get_field("0[k]", ({"k": "V"},), {}))
print(fmt.get_field("x.y", (), {"x": P("deep")}))
