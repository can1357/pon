import csv
for data in [[], [''], ['\n'], ['\r\n'], ['a,'], [',']]:
    try: print(repr(data), list(csv.reader(data)))
    except Exception as e: print(repr(data), type(e).__name__, str(e))
