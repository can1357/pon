import traceback
try:
    import typing
    print('ok')
except Exception:
    traceback.print_exc()
    raise
