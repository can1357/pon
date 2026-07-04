import ssl
print('has', hasattr(ssl, 'SSLSocket'), hasattr(ssl, 'SSLContext'), hasattr(ssl, '_create_stdlib_context'))
print('names', [n for n in ('SSLSocket','SSLContext','PROTOCOL_TLS_CLIENT','_create_stdlib_context') if hasattr(ssl,n)])
