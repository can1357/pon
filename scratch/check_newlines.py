p = '/tmp/pon_check_newlines.txt'
open(p, 'wb').write(b'a\r\nb\rc\n')
for nl in (None, '', '\n', '\r', '\r\n'):
    f = open(p, 'r', encoding='utf-8', newline=nl)
    print('mode', repr(nl), 'start', repr(f.newlines))
    print('line1', repr(f.readline()), repr(f.newlines))
    print('rest', repr(f.read()), repr(f.newlines))
    f.close()
