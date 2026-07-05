# subprocess's os.posix_spawn fast path: close_fds=False routes Popen through
# _posix_spawn, which passes file_actions=/setsigdef= keywords and env=None
# (inherit).  meson's Popen_safe exercises exactly this shape.
import os
import subprocess

p = subprocess.Popen(
    ['/bin/echo', 'spawned'],
    close_fds=False,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
out, err = p.communicate()
print(p.returncode, out.decode().strip(), err.decode())

p = subprocess.Popen(
    ['/bin/echo', 'text'],
    universal_newlines=True,
    encoding='UTF-8',
    close_fds=False,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
)
out, err = p.communicate(None)
print(p.returncode, out.strip())

# stderr=STDOUT hands one pipe fd to both slots 1 and 2 (meson's
# `cc -print-search-dirs` probe): the fork_exec path must dup2 both before
# closing the shared source, and the child's stderr must land on stdout.
p = subprocess.Popen(
    ['sh', '-c', 'echo to-out; echo to-err 1>&2'],
    universal_newlines=True,
    encoding='UTF-8',
    close_fds=False,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
)
out, err = p.communicate()
print(p.returncode, sorted(out.split()), err)

# Direct keyword surface: dup2/close file actions with env=None inheritance.
r, w = os.pipe()
pid = os.posix_spawn(
    '/bin/echo',
    ['/bin/echo', 'fa'],
    None,
    file_actions=[(os.POSIX_SPAWN_DUP2, w, 1), (os.POSIX_SPAWN_CLOSE, r)],
)
os.close(w)
print(os.read(r, 100).decode().strip())
os.waitpid(pid, 0)

# path/argv/env are positional-only.
try:
    os.posix_spawn(path='/bin/echo', argv=['/bin/echo'], env=None)
except TypeError:
    print('TypeError')
