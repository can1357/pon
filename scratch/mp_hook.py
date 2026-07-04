import os
os.chdir("/tmp/mp_min")
import mesonpy
try:
    print("get_requires:", mesonpy.get_requires_for_build_wheel())
except Exception as e:
    import traceback; traceback.print_exc()
