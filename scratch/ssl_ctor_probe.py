import ssl
print('imported')
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
print('ctx type', type(ctx), type(ctx).__mro__)
print('protocol', ctx.protocol)
ctx.check_hostname = False
print('set ok', ctx.check_hostname)
