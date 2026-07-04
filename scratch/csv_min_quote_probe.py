import csv, io
for kwargs in [{}, {'escapechar':'\\'}, {'doublequote':False,'escapechar':'\\'}, {'doublequote':False}]:
    s=io.StringIO()
    try:
        csv.writer(s, lineterminator='\n', **kwargs).writerow(['a"b'])
        print(kwargs, repr(s.getvalue()))
    except Exception as e: print(kwargs, type(e).__name__, str(e))
