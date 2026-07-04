import csv
for strict in [False, True]:
    try: print(strict, list(csv.reader(['"a'], strict=strict)))
    except Exception as e: print(strict, type(e).__name__, str(e))
