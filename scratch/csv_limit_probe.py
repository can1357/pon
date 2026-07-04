import _csv
for v in [None, 5, -1, 'x']:
    try:
        if v is None: print('none', _csv.field_size_limit())
        else: print(v, _csv.field_size_limit(v))
    except Exception as e: print(v, type(e).__name__, str(e))
