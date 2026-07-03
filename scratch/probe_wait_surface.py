import os

print(os.WNOHANG)
print(os.WIFSTOPPED(0), os.WIFSTOPPED(0x057F))
print(os.WSTOPSIG(0x137F), os.WSTOPSIG(0))
print(os.waitstatus_to_exitcode(0))
print(os.waitstatus_to_exitcode(0x0200))
print(os.waitstatus_to_exitcode(0x000F))
try:
    os.waitstatus_to_exitcode(0x027F)
except ValueError as exc:
    print("ValueError:", exc)
try:
    os.waitpid(-1, os.WNOHANG)
except ChildProcessError:
    print("ChildProcessError")

import subprocess

print(subprocess.PIPE, subprocess.STDOUT, subprocess.DEVNULL)
print(subprocess.SubprocessError.__name__)
print(issubclass(subprocess.CalledProcessError, subprocess.SubprocessError))
print(issubclass(subprocess.TimeoutExpired, subprocess.SubprocessError))
print(sorted(n for n in subprocess.__all__))
