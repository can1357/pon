import helper_mod
print(sorted(dir(helper_mod)))
print(sorted(vars(helper_mod).keys()) if isinstance(vars(helper_mod), dict) else "vars-not-dict")
print(vars(helper_mod) is helper_mod.__dict__)
helper_mod.extra = 1
print("extra" in dir(helper_mod))
