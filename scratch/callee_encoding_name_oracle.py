import tempfile
f=open(tempfile.mktemp(),'w+',encoding='utf-8')
f.reconfigure(encoding='UTF8')
print(f.encoding)
f.reconfigure(encoding='latin1')
print(f.encoding)
