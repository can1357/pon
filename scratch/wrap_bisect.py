import sys
sys.path.insert(0, '/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson')
for m in ['contextlib','dataclasses','urllib.request','urllib.error','urllib.parse','hashlib',
          'shutil','tempfile','stat','subprocess','configparser','textwrap','json','gzip',
          'base64','netrc','pathlib','functools']:
    __import__(m)
    print('ok', m)
