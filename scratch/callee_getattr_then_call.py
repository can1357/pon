import sys, functools
@functools.lru_cache()
def use():
    print('enter', getattr(sys.stdout, 'reconfigure', None))
    sys.stdout.reconfigure(errors='replace')
try:
    use()
except BaseException as e:
    print('error', type(e).__name__, str(e))
    raise
