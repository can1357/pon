import csv, io
for quoting in [csv.QUOTE_MINIMAL, csv.QUOTE_NONE, csv.QUOTE_STRINGS, csv.QUOTE_NOTNULL, csv.QUOTE_NONNUMERIC, csv.QUOTE_ALL]:
    s=io.StringIO()
    try:
        csv.writer(s, lineterminator='\n', quoting=quoting).writerow([''])
        print(quoting, repr(s.getvalue()))
    except Exception as e:
        print(quoting, type(e).__name__, str(e))
