import runpy
print(isinstance(__import__('sys').path_importer_cache, dict))
runpy.run_path('/work/pon/tmp/repro_runpy_target.py')
