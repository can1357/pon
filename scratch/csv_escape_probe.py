import csv, io
for text in ['a,b','a"b','a\\b']:
    s=io.StringIO();
    try:
        csv.writer(s, lineterminator='\n', doublequote=False, escapechar='\\').writerow([text])
        print(repr(text), repr(s.getvalue()))
    except Exception as e: print(repr(text), type(e).__name__, str(e))
