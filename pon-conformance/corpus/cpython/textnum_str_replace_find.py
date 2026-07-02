# Derived from CPython v3.14.0 Lib/test/test_str.py topics (PSF license).

def show_replace(label, text, old, new):
    print(label, text.replace(old, new))


def show_replace_count(label, text, old, new, count):
    print(label, text.replace(old, new, count))


def show_find(label, text, needle):
    print(label, text.find(needle))


show_replace("all", "one!two!three!", "!", "@")
show_replace_count("first", "one!two!three!", "!", "@", 1)
show_replace_count("zero", "one!two!three!", "!", "@", 0)
show_replace_count("negative", "banana", "na", "NA", -1)
show_replace("overlap", "aaaa", "aa", "b")
show_replace("unicode", "αβγβδ", "β", "-")
show_replace_count("unicode-one", "αβγβδ", "β", "-", 1)

show_find("front", "abracadabra", "abra")
show_find("middle", "abracadabra", "cad")
show_find("missing", "abracadabra", "xyz")
show_find("empty", "abracadabra", "")
show_find("unicode-hit", "тест", "т")
show_find("unicode-miss", "тест", "e")

print("prefix".startswith("pre"))
print("prefix".startswith("fix"))
print("suffix".endswith("fix"))
print("suffix".endswith("suf"))
