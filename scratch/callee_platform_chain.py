import sysconfig, os
print('fn', sysconfig.get_platform)
p = sysconfig.get_platform()
print('p1', repr(p), type(p), getattr(p, 'startswith', None), p.startswith('macosx-'))
if sysconfig.get_platform().startswith('macosx-'):
    print('mac')
elif sysconfig.get_platform().startswith('android-') and 'CIBUILDWHEEL' in os.environ:
    print('android')
elif sysconfig.get_platform().startswith('ios-'):
    print('ios')
else:
    print('none')
