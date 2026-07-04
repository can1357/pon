import csv, io
for value in [1, 1.5, True, False, '1', None]:
    s=io.StringIO()
    csv.writer(s, lineterminator='\n', quoting=csv.QUOTE_NONNUMERIC).writerow([value])
    print(type(value).__name__, repr(s.getvalue()))
print('strings')
for value in [1, 1.5, True, 'x', None]:
    s=io.StringIO(); csv.writer(s, lineterminator='\n', quoting=csv.QUOTE_STRINGS).writerow([value]); print(type(value).__name__, repr(s.getvalue()))
print('notnull')
for value in [1, 'x', None, '']:
    s=io.StringIO(); csv.writer(s, lineterminator='\n', quoting=csv.QUOTE_NOTNULL).writerow([value]); print(type(value).__name__, repr(s.getvalue()))
