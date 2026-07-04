import os
import sys
import tempfile

path = os.path.join(tempfile.gettempdir(), 'pon_reconfigure_probe.txt')
try:
    os.unlink(path)
except FileNotFoundError:
    pass

with open(path, 'w', encoding='utf-8') as f:
    f.reconfigure(errors='replace')
    f.write('\ud800')
    print('errors_attr', f.errors)
    f.reconfigure(line_buffering=True)
    print('line_buffering_attr', f.line_buffering)

with open(path, 'rb') as f:
    print('file_bytes', f.read())

sys.stdout.reconfigure(errors='replace')
print('stdout_reconfigured')

try:
    sys.stdout.reconfigure(errors=1)
except Exception as exc:
    print('errors_type_case', type(exc).__name__, str(exc))

try:
    sys.stdout.reconfigure('utf-8')
except Exception as exc:
    print('positional_case', type(exc).__name__, str(exc))
