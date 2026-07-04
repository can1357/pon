s = 'darwin'
m = s.startswith
print(type(m), repr(m))
print(m('macosx-'))
print(sysconfig_result := __import__('sysconfig').get_platform())
print(sysconfig_result.startswith('macosx-'))
