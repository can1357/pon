import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
for kwargs in [{'encoding':1},{'errors':1},{'newline':1}]:
    try: f.reconfigure(**kwargs)
    except BaseException as e: print(kwargs, type(e).__name__, str(e))
