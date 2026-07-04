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
    if absent:
        raise AssertionError((modname, absent))

import hashlib
assert hashlib.pbkdf2_hmac('sha256', b'password', b'salt', 1, 32).hex() == '120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b'
assert hashlib.scrypt(b'password', salt=b'NaCl', n=16, r=1, p=1, dklen=64).hex() == 'aec6b7483ed26e08802b41f4032086a0e886be7ac48fcfd92ff0cef8109752f4ac74b077263256a65a99701b7a304d46611c8aa391e799ce10a27753e7e9c09a'

from itertools import combinations_with_replacement
assert list(combinations_with_replacement('ABC', 2)) == [('A','A'),('A','B'),('A','C'),('B','B'),('B','C'),('C','C')]

import _collections
od = _collections.OrderedDict([('a', 1), ('b', 2)])
assert list(od.items()) == [('a', 1), ('b', 2)]
counts = {}
_collections._count_elements(counts, 'aba')
assert counts == {'a': 2, 'b': 1}
assert list(reversed(_collections.deque([1,2,3]))) == [3,2,1]

import _csv, csv
csv.register_dialect('pon_smoke', delimiter=';')
assert 'pon_smoke' in _csv._dialects
assert _csv.get_dialect('pon_smoke').delimiter == ';'

import _pickle, pickle
blob = _pickle.dumps({'x': [1, 2]})
assert _pickle.loads(blob) == {'x': [1, 2]}
assert _pickle.Pickler is pickle.Pickler

import _warnings, warnings
_warnings._acquire_lock(); _warnings._release_lock()
with warnings._Lock():
    pass

import gc
gc.set_debug(gc.DEBUG_STATS)
assert gc.get_debug() == gc.DEBUG_STATS
gc.set_threshold(11, 12, 13)
assert gc.get_threshold() == (11, 12, 13)
assert gc.get_count() == (0, 0, 0)
assert len(gc.get_stats()) == 3
assert gc.get_freeze_count() == 0

import _thread
assert _thread.allocate is _thread.allocate_lock
assert _thread.start_new is _thread.start_new_thread
assert isinstance(_thread.get_native_id(), int)
old_name = _thread._get_name()
_thread.set_name('pon-smoke')
assert _thread._get_name() == 'pon-smoke'
_thread.set_name(old_name)
try:
    _thread.interrupt_main()
except KeyboardInterrupt:
    pass
else:
    raise AssertionError('interrupt_main did not raise')

import _colorize
assert _colorize.default_theme.syntax.keyword
_colorize.set_theme(_colorize.theme_no_color)
assert _colorize.get_theme(force_color=True) is _colorize.theme_no_color
_colorize.set_theme(_colorize.default_theme)
print('existing_native_attrs_smoke OK')
