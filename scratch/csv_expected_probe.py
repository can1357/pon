import csv, io
rows = [
    ['plain', 'comma,inside', 'quote"inside', 'line\nbreak', None, ''],
    ['', 'tail'],
]
s = io.StringIO()
w = csv.writer(s, delimiter=',', quotechar='"', lineterminator='\n')
w.writerows(rows)
print(repr(s.getvalue()))
qa = io.StringIO(); csv.writer(qa, lineterminator='\n', quoting=csv.QUOTE_ALL).writerow(['a','b,c']); print(repr(qa.getvalue()))
qn = io.StringIO(); csv.writer(qn, lineterminator='\n', quoting=csv.QUOTE_NONE, escapechar='\\').writerow(['a,b','c"d','line\nbreak']); print(repr(qn.getvalue()))
print(list(csv.reader(io.StringIO(s.getvalue()), delimiter=',', quotechar='"')))
