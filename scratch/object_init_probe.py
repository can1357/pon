print('start')
for label, func in [
    ('object_instance_init', lambda: object().__init__()),
    ('plain_class_construct', lambda: type('A', (), {})()),
    ('direct_object_init_noarg', lambda: object.__init__()),
]:
    print('CASE', label)
    try:
        result = func()
        print('OK', result)
    except BaseException as exc:
        print('ERR', type(exc).__name__, str(exc))
