l = [3,1,2]
for name in ['append','extend','insert','remove','pop','clear','index','count','sort','reverse','copy','__contains__','__len__']:
    print(name, hasattr(l, name))
