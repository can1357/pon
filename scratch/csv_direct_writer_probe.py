import _csv, io
for kwargs in [{}, {'dialect':'excel'}]:
    try:
        s=io.StringIO(); _csv.writer(s, **kwargs).writerow(['a']); print(kwargs, repr(s.getvalue()))
    except Exception as e: print(kwargs, type(e).__name__, str(e))
