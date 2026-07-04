import sys
print('stdout', repr(sys.stdout), type(sys.stdout), getattr(sys.stdout, 'reconfigure', 'DEFAULT'))
