import _thread

workers = 4
iterations = 25
counter = 0
counter_lock = _thread.allocate_lock()
done0 = _thread.allocate_lock()
done1 = _thread.allocate_lock()
done2 = _thread.allocate_lock()
done3 = _thread.allocate_lock()

done0.acquire()
done1.acquire()
done2.acquire()
done3.acquire()


def worker(done_lock):
    global counter
    for i in range(iterations):
        counter_lock.acquire()
        counter = counter + 1
        counter_lock.release()
    done_lock.release()


_thread.start_new_thread(worker, (done0,))
_thread.start_new_thread(worker, (done1,))
_thread.start_new_thread(worker, (done2,))
_thread.start_new_thread(worker, (done3,))

done0.acquire()
done1.acquire()
done2.acquire()
done3.acquire()

print("shared_counter ok", counter)
