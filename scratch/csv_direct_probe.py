import _csv
print(_csv.list_dialects())
try: print(_csv.get_dialect('excel'))
except Exception as e: print(type(e).__name__, str(e))
