try:
    b"\xff".decode("utf-8", "surrogateescape")
except UnicodeDecodeError as e:
    print("UDE:", e)
except LookupError as e:
    print("LookupError:", e)
