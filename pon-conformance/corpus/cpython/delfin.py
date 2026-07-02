import gc


deleted = False


class Finalized:
    def __del__(self):
        global deleted
        deleted = True


obj = Finalized()
del obj
gc.collect()
gc.collect()
print("deleted", deleted)
