import sys
print('getattr-default', getattr(sys.stdout, 'reconfigure', 'DEFAULT'))
try:
    print('direct-attr', sys.stdout.reconfigure)
except BaseException as e:
    print('direct-attr-error', type(e).__name__, str(e))
try:
    sys.stdout.reconfigure(errors='replace')
except BaseException as e:
    print('direct-call-error', type(e).__name__, str(e))
