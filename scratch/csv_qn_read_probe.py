import csv, io
text='a\\,b,c\\"d,line\\\nbreak\n'
print(repr(text))
print(list(csv.reader(io.StringIO(text), quoting=csv.QUOTE_NONE, escapechar='\\')))
