p='/work/pon/tmp/io_method_shadow_data.txt'
open(p,'w').write('abc')
with open(p, 'r+', encoding='utf-8') as f:
    f.read = 'shadow'
    print('read callable', callable(f.read))
    print('read data', f.read(1))
    del f.read
    print('read callable after del', callable(f.read))
