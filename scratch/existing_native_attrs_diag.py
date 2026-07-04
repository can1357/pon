def step(name, func):
    print('STEP', name)
    func()
    print('OK', name)

def attrs():
    missing = {
        '_blake2': ['BLAKE2B_MAX_DIGEST_SIZE','BLAKE2B_MAX_KEY_SIZE','BLAKE2B_PERSON_SIZE','BLAKE2B_SALT_SIZE','BLAKE2S_MAX_DIGEST_SIZE','BLAKE2S_MAX_KEY_SIZE','BLAKE2S_PERSON_SIZE','BLAKE2S_SALT_SIZE','_GIL_MINSIZE'],
        '_collections': ['OrderedDict','_count_elements','_deque_iterator','_deque_reverse_iterator','_tuplegetter'],
        '_colorize': ['Argparse','Callable','ColorCodes','Field','Iterator','Mapping','NoColors','Syntax','Theme','ThemeSection','Traceback','Unittest','__annotate__','__conditional_annotations__','_theme','attr','code','dataclass','default_theme','field','os','set_theme','sys','theme_no_color'],
        '_csv': ['Reader','Writer','_dialects'],
        '_md5': ['MD5Type','_GIL_MINSIZE'],
        '_pickle': ['PickleError','Pickler','PicklingError','Unpickler','UnpicklingError','dump','dumps','load','loads'],
        '_sha1': ['SHA1Type','_GIL_MINSIZE'],
        '_sha2': ['SHA224Type','SHA256Type','SHA384Type','SHA512Type','_GIL_MINSIZE'],
        '_thread': ['_NAME_MAXLEN','_count','_get_name','allocate','exit','exit_thread','get_native_id','interrupt_main','lock','set_name','start_new'],
        '_warnings': ['_acquire_lock','_defaultaction','_onceregistry','_release_lock'],
        'gc': ['DEBUG_COLLECTABLE','DEBUG_LEAK','DEBUG_SAVEALL','DEBUG_STATS','DEBUG_UNCOLLECTABLE','callbacks','freeze','garbage','get_count','get_debug','get_freeze_count','get_objects','get_referents','get_referrers','get_stats','get_threshold','is_finalized','is_tracked','set_debug','set_threshold','unfreeze'],
        'hashlib': ['pbkdf2_hmac','scrypt'],
        'itertools': ['_grouper','_tee','_tee_dataobject','combinations_with_replacement'],
        'warnings': ['_Lock','_acquire_lock','_release_lock'],
    }
    for modname, names in missing.items():
        mod = __import__(modname)
        absent = [name for name in names if not hasattr(mod, name)]
        print(modname, absent)
        assert not absent

def hash_tests():
    import hashlib
    print(hashlib.pbkdf2_hmac('sha256', b'password', b'salt', 1, 32).hex())
    print(hashlib.scrypt(b'password', salt=b'NaCl', n=16, r=1, p=1, dklen=64).hex())

def iter_tests():
    from itertools import combinations_with_replacement
    print(list(combinations_with_replacement('ABC', 2)))
    assert list(combinations_with_replacement('ABC', 2)) == [('A','A'),('A','B'),('A','C'),('B','B'),('B','C'),('C','C')]

def collections_tests():
    import _collections
    print(_collections.OrderedDict)
    od = _collections.OrderedDict([('a', 1), ('b', 2)])
    print(list(od.items()))
    counts = {}
    _collections._count_elements(counts, 'aba')
    print(counts)
    print(list(reversed(_collections.deque([1,2,3]))))

def csv_tests():
    import _csv, csv
    csv.register_dialect('pon_smoke', delimiter=';')
    print(_csv._dialects)
    print('pon_smoke' in _csv._dialects)
    print(_csv.get_dialect('pon_smoke').delimiter)

def pickle_tests():
    import _pickle, pickle
    blob = _pickle.dumps({'x': [1, 2]})
    print(_pickle.loads(blob))
    print(_pickle.Pickler is pickle.Pickler)

def warnings_tests():
    import _warnings, warnings
    _warnings._acquire_lock(); _warnings._release_lock()
    with warnings._Lock(): pass

def gc_tests():
    import gc
    gc.set_debug(gc.DEBUG_STATS)
    print(gc.get_debug(), gc.DEBUG_STATS)
    gc.set_threshold(11,12,13)
    print(gc.get_threshold(), gc.get_count(), len(gc.get_stats()), gc.get_freeze_count())

def thread_tests():
    import _thread
    print(_thread.allocate is _thread.allocate_lock, _thread.start_new is _thread.start_new_thread, _thread.get_native_id(), _thread._get_name())
    old = _thread._get_name()
    _thread.set_name('pon-smoke')
    print(_thread._get_name())
    _thread.set_name(old)
    try: _thread.interrupt_main()
    except KeyboardInterrupt: print('ki')

def colorize_tests():
    import _colorize
    print(_colorize.default_theme.syntax.keyword)
    _colorize.set_theme(_colorize.theme_no_color)
    print(_colorize.get_theme(force_color=True) is _colorize.theme_no_color)
    _colorize.set_theme(_colorize.default_theme)

for name, func in [('attrs', attrs), ('hash', hash_tests), ('iter', iter_tests), ('collections', collections_tests), ('csv', csv_tests), ('pickle', pickle_tests), ('warnings', warnings_tests), ('gc', gc_tests), ('thread', thread_tests), ('colorize', colorize_tests)]:
    step(name, func)
print('DIAG DONE')
