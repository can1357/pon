import os
import _posixsubprocess
import subprocess

# --- the import rungs themselves --------------------------------------------------
# subprocess's POSIX branch binds _posixsubprocess.fork_exec at module scope;
# calling it is deliberately NOT probed (real spawn on CPython, honest
# NotImplementedError in pon).
print(callable(_posixsubprocess.fork_exec))
print(callable(subprocess._fork_exec))
print(subprocess._mswindows, subprocess._can_fork_exec)

# --- os wait surface read by subprocess._del_safe at import time -------------------
print(os.WNOHANG)
print(subprocess._del_safe.WNOHANG, subprocess._del_safe.ECHILD)
print(callable(subprocess._del_safe.waitpid), callable(subprocess._del_safe.WIFSTOPPED))
print(os.WIFSTOPPED(0), os.WIFSTOPPED(0x057F))
print(os.WSTOPSIG(0x137F), os.WSTOPSIG(0))

# --- waitstatus_to_exitcode: pure status-word math (portable inputs only) ----------
print(os.waitstatus_to_exitcode(0))
print(os.waitstatus_to_exitcode(0x0200))
print(os.waitstatus_to_exitcode(0x000F))
try:
    os.waitstatus_to_exitcode(0x027F)
except ValueError as exc:
    print("ValueError:", exc)

# --- waitpid: this process has no children on either runtime -----------------------
try:
    os.waitpid(-1, os.WNOHANG)
except ChildProcessError:
    print("ChildProcessError")

# --- module surface -----------------------------------------------------------------
print(subprocess.PIPE, subprocess.STDOUT, subprocess.DEVNULL)
print(subprocess.SubprocessError.__name__)
print(issubclass(subprocess.CalledProcessError, subprocess.SubprocessError))
print(issubclass(subprocess.TimeoutExpired, subprocess.SubprocessError))
print(sorted(subprocess.__all__))
