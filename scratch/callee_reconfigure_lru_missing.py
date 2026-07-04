import sys, functools
@functools.lru_cache()
def use():
    print('before', getattr(sys.stdout, 'reconfigure', None))
    sys.stdout.reconfigure(errors='replace')
    print('after')
try:
    use()
except BaseException as e:
    print('error', type(e).__name__, str(e))
    raise
