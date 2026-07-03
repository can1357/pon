import gc


deleted = False


class Finalized:
    def __del__(self):
        global deleted
        deleted = True


def g():
    obj = Finalized()
    del obj
    gc.collect()
    gc.collect()


g()
print("fn-scope", deleted)
