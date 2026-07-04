import pathlib, subprocess, traceback
print('first')
subprocess.run(['/bin/echo', 'one'], text=True, capture_output=True)
print('second')
subprocess.run(['/bin/echo', 'two'], check=False, text=True, capture_output=True)
print('third')
try:
    subprocess.run(['/Users/can/.cache/cargo-target/debug/pon-cli', '/work/pon/tmp/child_ok.py', 'setup', '/private/tmp/numpy_src/numpy-2.5.0', '/private/tmp/numpy_src/numpy-2.5.0/.mesonpy-x', '-Dbuildtype=release', '-Db_ndebug=if-release', '-Db_vscrt=md', '--native-file=/private/tmp/numpy_src/numpy-2.5.0/.mesonpy-x/meson-python-native-file.ini'], cwd=pathlib.Path('/tmp'))
except BaseException:
    traceback.print_exc()
    raise
