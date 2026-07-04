import importlib
mods = ['_json','_functools','pyexpat','_hashlib','_multibytecodec','_io','_sre','zoneinfo','unittest','importlib']
targets = {
    '_json': ['make_encoder','make_scanner','encode_basestring','scanstring'],
    '_functools': ['partial','Placeholder','_PlaceholderType','_lru_cache_wrapper','reduce','cmp_to_key'],
    'pyexpat': ['XMLParserType','features','expat_CAPI','ParserCreate'],
    '_hashlib': ['_constructors','openssl_md5','openssl_sha256'],
    '_multibytecodec': ['__create_codec'],
    '_io': ['_BytesIOBuffer','BytesIO'],
    '_sre': ['copyright'],
    'zoneinfo': ['TZPATH','ZoneInfo'],
    'unittest': ['IsolatedAsyncioTestCase','TestCase'],
    'importlib': ['_abc','machinery','util','import_module'],
}
for name in mods:
    try:
        mod = importlib.import_module(name)
    except Exception as exc:
        print(name, 'IMPORT_ERR', type(exc).__name__, exc)
        continue
    print('MOD', name)
    d = dir(mod)
    for attr in targets[name]:
        print(attr, attr in d, hasattr(mod, attr), type(getattr(mod, attr, None)).__name__)
