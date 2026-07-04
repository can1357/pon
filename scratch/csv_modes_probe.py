import csv, io
for mode in ['QUOTE_STRINGS','QUOTE_NOTNULL']:
    print(mode)
    for value in [1, 1.5, True, 'x', None, '']:
        s=io.StringIO()
        try:
            csv.writer(s, lineterminator='\n', quoting=getattr(csv, mode)).writerow([value])
            print(type(value).__name__, repr(s.getvalue()))
        except Exception as e:
            print(type(value).__name__, type(e).__name__, str(e))
