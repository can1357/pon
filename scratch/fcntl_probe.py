import fcntl, os, tempfile
print(fcntl.LOCK_EX, fcntl.LOCK_NB, fcntl.LOCK_SH, fcntl.LOCK_UN)
path = tempfile.mktemp()
f = open(path, 'w+')
fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
print('locked')
fcntl.flock(f.fileno(), fcntl.LOCK_UN)
print('unlocked')
# conflicting lock from a second fd -> BlockingIOError
f2 = open(path, 'w+')
fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
try:
    fcntl.flock(f2, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError as e:
    print('BlockingIOError errno', e.errno == 35 or e.errno == 11)
try:
    fcntl.flock('x', fcntl.LOCK_UN)
except TypeError as e:
    print('TypeError:', e)
f.close(); f2.close(); os.unlink(path)
print('done')
