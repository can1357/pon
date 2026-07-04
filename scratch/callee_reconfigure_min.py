import sys
sys.stdout.write(sys.stdout.errors + '\n')
sys.stdout.reconfigure(errors='replace')
sys.stdout.write(sys.stdout.errors + '\n')
sys.stdout.reconfigure(encoding='ascii')
sys.stdout.write(sys.stdout.errors + '\n')
sys.stdout.reconfigure(errors='replace', line_buffering=True, write_through=True)
sys.stdout.write(sys.stdout.encoding + ' ' + sys.stdout.errors + ' ' + str(sys.stdout.line_buffering) + ' ' + str(sys.stdout.write_through) + '\n')
sys.stdout.write('é\n')
