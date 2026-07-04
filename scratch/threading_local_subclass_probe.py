import threading

class LocalWithDefault(threading.local):
    def __init__(self):
        self.default = "default"

local = LocalWithDefault()
print("main", local.default)
results = []

def worker(label):
    results.append((label, "before", hasattr(local, "default"), local.default))
    local.default = label
    results.append((label, "after", local.default))

threads = [threading.Thread(target=worker, args=("alpha",)), threading.Thread(target=worker, args=("beta",))]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
print("results", sorted(results))
print("main_after", local.default)
