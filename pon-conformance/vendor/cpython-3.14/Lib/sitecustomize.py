# This file is created by Homebrew and is executed on each python startup.
# Don't print from here, or else python command line scripts may fail!
# <https://docs.brew.sh/Homebrew-and-Python>
import re
import os
import site
import sys
if sys.version_info[:2] != (3, 14):
    # This can only happen if the user has set the PYTHONPATH to a mismatching site-packages directory.
    # Every Python looks at the PYTHONPATH variable and we can't fix it here in sitecustomize.py,
    # because the PYTHONPATH is evaluated after the sitecustomize.py. Many modules (e.g. PyQt4) are
    # built only for a specific version of Python and will fail with cryptic error messages.
    # In the end this means: Don't set the PYTHONPATH permanently if you use different Python versions.
    exit(f'Your PYTHONPATH points to a site-packages dir for Python 3.14 '
         f'but you are running Python {sys.version_info[0]}.{sys.version_info[1]}!\n'
         f'     PYTHONPATH is currently: "{os.environ["PYTHONPATH"]}"\n'
         f'     You should `unset PYTHONPATH` to fix this.')
# Only do this for a brewed python:
if os.path.realpath(sys.executable).startswith('/opt/homebrew/Cellar/python@3.14'):
    # Shuffle /Library site-packages to the end of sys.path
    library_site = '/Library/Python/3.14/site-packages'
    library_packages = [p for p in sys.path if p.startswith(library_site)]
    sys.path = [p for p in sys.path if not p.startswith(library_site)]
    # .pth files have already been processed so don't use addsitedir
    sys.path.extend(library_packages)
    # the Cellar site-packages is a symlink to the HOMEBREW_PREFIX
    # site_packages; prefer the shorter paths
    long_prefix = re.compile(r'/opt/homebrew/Cellar/python@3.14/[0-9\._abrc]+/(?:Frameworks/Python\.framework/Versions/3.14/)?lib/python3.14/site-packages')
    sys.path = [long_prefix.sub('/opt/homebrew/lib/python3.14/site-packages', p) for p in sys.path]
    # Set the sys.executable to use the opt_prefix. Only do this if PYTHONEXECUTABLE is not
    # explicitly set and we are not in a virtualenv:
    if 'PYTHONEXECUTABLE' not in os.environ and sys.prefix == sys.base_prefix:
        sys.executable = sys._base_executable = '/opt/homebrew/opt/python@3.14/bin/python3.14'
if 'PYTHONHOME' not in os.environ:
    cellar_prefix = re.compile(r'/opt/homebrew/Cellar/python@3.14/[0-9\._abrc]+(?=/|$)')
    if os.path.realpath(sys.base_prefix).startswith('/opt/homebrew/Cellar/python@3.14'):
        new_prefix = cellar_prefix.sub('/opt/homebrew/opt/python@3.14', sys.base_prefix)
        site.PREFIXES[:] = [new_prefix if x == sys.base_prefix else x for x in site.PREFIXES]
        if sys.prefix == sys.base_prefix:
            sys.prefix = new_prefix
        sys.base_prefix = new_prefix
    if os.path.realpath(sys.base_exec_prefix).startswith('/opt/homebrew/Cellar/python@3.14'):
        new_exec_prefix = cellar_prefix.sub('/opt/homebrew/opt/python@3.14', sys.base_exec_prefix)
        site.PREFIXES[:] = [new_exec_prefix if x == sys.base_exec_prefix else x for x in site.PREFIXES]
        if sys.exec_prefix == sys.base_exec_prefix:
            sys.exec_prefix = new_exec_prefix
        sys.base_exec_prefix = new_exec_prefix
# Make site.getsitepackages() return the HOMEBREW_PREFIX site-packages,
# but only if we are outside a venv or in one with include-system-site-packages.
if sys.base_prefix in site.PREFIXES:
    site.PREFIXES.insert(site.PREFIXES.index(sys.base_prefix), '/opt/homebrew')
    site.addsitedir('/opt/homebrew/lib/python3.14/site-packages')
# Check for and add the prefix of split Python formulae.
for split_module in ["tk", "gdbm"]:
    split_prefix = f"/opt/homebrew/opt/python-{split_module}@3.14/libexec"
    if os.path.isdir(split_prefix):
        sys.path.append(split_prefix)
