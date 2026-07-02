name = "pon"
value = 7
w = 4

template = t"pre {name!r} {value:{w}d} {value=}"
print(type(template).__name__)
print(template.strings)
print(template.values)
for item in template.interpolations:
    print(item.value, item.expression, item.conversion, item.format_spec)

joined = t"A{name}" + t"B{value}"
print(joined.strings)
print(joined.values)

implicit = t"{1}" t"{2}"
print(implicit.strings)
print(implicit.values)
