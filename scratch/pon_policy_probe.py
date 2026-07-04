source = open("/work/pon/tmp/policybase_nodoc.py").read()
ns = {"__name__": "policy_probe"}
exec(source, ns)
Compat32 = ns["Compat32"]
for name, attr in Compat32.__dict__.items():
    try:
        doc = attr.__doc__
        print(name, type(attr).__name__, doc is None, repr(doc)[:40])
    except Exception as exc:
        print("FAIL", name, type(attr).__name__, type(exc).__name__, exc)
