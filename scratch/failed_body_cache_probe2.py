import sys
for i in range(2):
    try:
        import broken_cached
    except Exception:
        print('caught', i)
    print('cached', i, 'broken_cached' in sys.modules)
print('OK')
