import csv
for kwargs in [{}, {'delimiter': ';'}]:
    try:
        csv.register_dialect('x', **kwargs)
        d=csv.get_dialect('x')
        print(kwargs, d.delimiter, d.quotechar, d.quoting)
        csv.unregister_dialect('x')
    except Exception as e: print(kwargs, type(e).__name__, str(e))
