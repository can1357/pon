try:
    try:
        raise ExceptionGroup("g", [ValueError("v"), TypeError("t"), KeyError("k")])
    except* ValueError as exc:
        print("value", type(exc).__name__, [type(item).__name__ for item in exc.exceptions])
    except* TypeError as exc:
        print("type", [type(item).__name__ for item in exc.exceptions])
except ExceptionGroup as rest:
    print("rest", rest.message, [type(item).__name__ for item in rest.exceptions])

try:
    raise GeneratorExit()
except Exception:
    print("bad catch")
except BaseException as exc:
    print("base catch", type(exc).__name__)

try:
    raise ValueError("naked")
except* ValueError as exc:
    print("naked", type(exc).__name__, [type(item).__name__ for item in exc.exceptions])
