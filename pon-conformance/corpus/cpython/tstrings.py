name = "pon"
value = 5
template = t"hello {name} {value + 1}"
print(type(template).__name__)
print(template.strings)
print([item.value for item in template.interpolations])
