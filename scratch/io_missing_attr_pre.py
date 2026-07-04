p = '/work/pon/tmp/io_missing_attr_pre_data.txt'
open(p, 'w').close()
with open(p, 'r+', encoding='utf-8') as f:
    print(getattr(f, 'nope', 'default'))
    print(hasattr(f, 'nope'))
    try:
        f.nope
    except Exception as e:
        print(type(e).__name__)
