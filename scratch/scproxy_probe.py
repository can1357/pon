import _scproxy
s = _scproxy._get_proxy_settings()
p = _scproxy._get_proxies()
print(sorted(s), type(s).__name__)
print(sorted(p), type(p).__name__)
