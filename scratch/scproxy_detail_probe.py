import _scproxy
print(repr(_scproxy._get_proxy_settings()))
print(repr(_scproxy._get_proxies()))
for k, v in sorted(_scproxy._get_proxy_settings().items()):
    print(k, type(v).__name__, repr(v))
for k, v in sorted(_scproxy._get_proxies().items()):
    print(k, type(v).__name__, repr(v))
