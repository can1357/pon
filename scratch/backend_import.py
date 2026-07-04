import packaging.requirements
print("packaging.requirements ok")
r = packaging.requirements.Requirement("numpy>=1.0; python_version >= '3.8'")
print("requirement parse ok:", r.name, str(r.specifier))
import packaging.version
print("version parse ok:", packaging.version.Version("2.4.0.dev0"))
import pyproject_metadata
print("pyproject_metadata ok")
import mesonpy
print("mesonpy ok:", hasattr(mesonpy, "build_wheel"), hasattr(mesonpy, "get_requires_for_build_wheel"))
