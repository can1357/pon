def f():
    try:
        try:
            with open('/tmp/pon_definitely_missing_coredata_probe', 'rb') as fh:
                print('opened')
        except FileNotFoundError:
            print('inner caught')
    except Exception as e:
        print('outer caught', type(e).__name__)
    finally:
        print('finally')

f()
print('done')
