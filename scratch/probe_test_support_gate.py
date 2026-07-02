try:
    import test.support as s
    print('test.support OK')
    print('Py_GIL_DISABLED:', s.Py_GIL_DISABLED)
    print('TEST_MODULES_ENABLED:', s.TEST_MODULES_ENABLED)
    print('check_sanitizer:', s.check_sanitizer(address=True, memory=True, ub=True, thread=True))
except BaseException as exc:
    print('GATE:', type(exc).__name__, exc)
