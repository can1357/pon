import csv
import io

rows = [
    ['plain', 'comma,inside', 'quote"inside', 'line\nbreak', None, ''],
    ['', 'tail'],
]
expected_text = 'plain,"comma,inside","quote""inside","line\nbreak",,\n,tail\n'

s = io.StringIO()
w = csv.writer(s, delimiter=',', quotechar='"', lineterminator='\n')
ret = w.writerows(rows)
assert ret is None
text = s.getvalue()
assert text == expected_text, (repr(text), repr(expected_text))
print('writer', repr(text))

read_rows = list(csv.reader(io.StringIO(text), delimiter=',', quotechar='"'))
expected_rows = [
    ['plain', 'comma,inside', 'quote"inside', 'line\nbreak', '', ''],
    ['', 'tail'],
]
assert read_rows == expected_rows, (read_rows, expected_rows)
print('reader', read_rows)

all_buf = io.StringIO()
csv.writer(all_buf, lineterminator='\n', quoting=csv.QUOTE_ALL).writerow(['a', 'b,c'])
quote_all_text = all_buf.getvalue()
assert quote_all_text == '"a","b,c"\n', repr(quote_all_text)
print('quote_all', repr(quote_all_text))

none_buf = io.StringIO()
csv.writer(none_buf, lineterminator='\n', quoting=csv.QUOTE_NONE, escapechar='\\').writerow(['a,b', 'c"d', 'line\nbreak'])
quote_none_text = none_buf.getvalue()
expected_none = 'a\\,b,c\\"d,line\\\nbreak\n'
assert quote_none_text == expected_none, (repr(quote_none_text), repr(expected_none))
quote_none_rows = list(csv.reader(io.StringIO(quote_none_text), quoting=csv.QUOTE_NONE, escapechar='\\'))
assert quote_none_rows == [['a,b', 'c"d', 'line\nbreak']], quote_none_rows
print('quote_none', repr(quote_none_text), quote_none_rows)

print('csv smoke ok')
