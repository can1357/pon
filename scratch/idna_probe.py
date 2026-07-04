import encodings.idna
print('x'.encode('idna'))
print('bücher.example'.encode('idna'))
import stringprep
print(stringprep.in_table_a1('\u0221'))
