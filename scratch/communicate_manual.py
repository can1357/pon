import subprocess, selectors, os
p = subprocess.Popen(['/bin/echo','hi'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
stdout=[]; stderr=[]
sel = selectors.PollSelector()
sel.register(p.stdout, selectors.EVENT_READ)
sel.register(p.stderr, selectors.EVENT_READ)
print('registered', flush=True)
try:
    while sel.get_map():
        print('loop map', sel.get_map(), flush=True)
        ready = sel.select(None)
        print('ready', ready, flush=True)
        for key, events in ready:
            print('key', key, events, flush=True)
            data = os.read(key.fd, 32768)
            print('data', repr(data), flush=True)
            if not data:
                sel.unregister(key.fileobj)
                key.fileobj.close()
            if key.fileobj is p.stdout:
                stdout.append(data)
            else:
                stderr.append(data)
    print('wait', p.wait(), stdout, stderr, flush=True)
except BaseException as exc:
    print('caught', type(exc).__name__, repr(exc), getattr(exc,'args',None), flush=True)
