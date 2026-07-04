import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
for call in [lambda: f.reconfigure('ascii'), lambda: f.reconfigure('ascii','replace')]:
    try:
        call()
    except BaseException as e:
        print(type(e).__name__, str(e))
