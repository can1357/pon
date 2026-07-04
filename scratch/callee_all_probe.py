value = ['--tags=runtime,python-runtime,tests,devel']
print(type(all), repr(all))
print(type(isinstance), repr(isinstance))
print(type(str), repr(str))
print(not isinstance(value, list) or not all(isinstance(x, str) for x in value))
