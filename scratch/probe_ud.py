import unicodedata as u
print(u.unidata_version)
print(u.normalize("NFD", "\u00e0"), len(u.normalize("NFD", "\u00e0")))
print(u.category("A"), u.combining("\u0301"), u.east_asian_width("\u4e00"))
print(u.decimal("7"), u.digit("\u2460"), u.numeric("\u00bd"))
