import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
for name in ['line_buffering','write_through']:
    try:
        f.reconfigure(**{name: None})
        print(name, 'None OK', getattr(f,name))
    except BaseException as e:
        print(name, 'None ERR', type(e).__name__, str(e))
    try:
        f.reconfigure(**{name: 0})
        print(name, '0 OK', getattr(f,name))
    except BaseException as e:
        print(name, '0 ERR', type(e).__name__, str(e))
