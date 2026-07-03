import gc


deleted = False


class Finalized:
    def __del__(self):
        global deleted
        deleted = True


obj = Finalized()
print("addr", hex(id(obj)))
del obj
gc.collect()
gc.collect()
print("deleted", deleted)
