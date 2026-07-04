import tempfile, os, sys

def show(label, func):
    try:
        r = func()
        print(label, 'OK', repr(r))
    except BaseException as e:
        print(label, 'ERR', type(e).__name__, str(e))

p = tempfile.mktemp()
f = open(p, 'w+', encoding='utf-8', newline='\n')
print('initial', f.encoding, f.errors, f.line_buffering, f.write_through, f.newlines)
show('errors_replace', lambda: f.reconfigure(errors='replace'))
print('after_errors', f.encoding, f.errors, f.line_buffering, f.write_through)
show('ascii_replace', lambda: f.reconfigure(encoding='ascii', errors='replace'))
print('after_ascii', f.encoding, f.errors, f.line_buffering, f.write_through)
show('write_nonascii', lambda: f.write('aé\n'))
f.flush(); f.seek(0); print('bytes', open(p,'rb').read())
show('line_true', lambda: f.reconfigure(line_buffering=True))
print('after_line', f.line_buffering, f.write_through)
show('write_through_true', lambda: f.reconfigure(write_through=True))
print('after_write_through', f.line_buffering, f.write_through)
show('bad_encoding_none_errors', lambda: f.reconfigure(encoding=None, errors='replace'))
show('bad_newline', lambda: f.reconfigure(newline='x'))
show('bad_encoding', lambda: f.reconfigure(encoding='not-a-codec'))
show('bad_errors', lambda: f.reconfigure(errors='bogus'))
f.close()
show('closed', lambda: f.reconfigure(errors='replace'))
