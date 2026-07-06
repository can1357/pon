import threading


def worker():
    print("worker")


t = threading.Thread(target=worker)
print("start")
t.start()
print("join")
t.join()
print("done")
