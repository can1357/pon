import threading

local = threading.local()
print("start_hasattr", hasattr(local, "value"))
print("getattr_default", getattr(local, "value", "fallback"))

local.value = "first"
print("plain_set", local.value)
local.value = "second"
print("plain_overwrite", local.value)

results = []


def worker(label):
    before = hasattr(local, "value")
    local.value = label
    after = local.value
    results.append((label, before, after))


threads = [
    threading.Thread(target=worker, args=("alpha",)),
    threading.Thread(target=worker, args=("beta",)),
]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
print("thread_results", sorted(results))
print("main_after_threads", local.value)

del local.value
print("after_del_hasattr", hasattr(local, "value"))
print("after_del_getattr", getattr(local, "value", "fallback_after_del"))
try:
    local.value
except AttributeError as exc:
    print("after_del", type(exc).__name__)
else:
    print("after_del", "NO_ERROR")

try:
    del local.value
except AttributeError as exc:
    print("del_missing", type(exc).__name__)
else:
    print("del_missing", "NO_ERROR")

fresh = threading.local()
fresh_results = []


def fresh_worker():
    fresh_results.append(hasattr(fresh, "value"))


fresh_thread = threading.Thread(target=fresh_worker)
fresh_thread.start()
fresh_thread.join()
print("fresh_thread_hasattr", fresh_results[0])
