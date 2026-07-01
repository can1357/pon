def probe(value):
    try:
        if value < 0:
            raise ValueError("negative")
    except ValueError as exc:
        print("except", type(exc).__name__)
    else:
        print("else", value)
    finally:
        print("finally", value)

probe(3)
probe(-1)
