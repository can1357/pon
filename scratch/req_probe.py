def show(label, fn):
    try:
        print(label, "=>", fn())
    except Exception as e:
        print(label, "ERR", type(e).__name__, repr(str(e)))

import packaging.requirements as R
show("Requirement numpy", lambda: str(R.Requirement("numpy")))
show("Requirement numpy>=1.0", lambda: str(R.Requirement("numpy>=1.0")))
show("Requirement extras", lambda: str(R.Requirement("numpy[test]>=1.0")))
show("Requirement marker", lambda: str(R.Requirement("numpy; python_version >= '3.8'")))
show("Requirement full", lambda: str(R.Requirement("numpy>=1.0; python_version >= '3.8'")))

import packaging.markers as M
show("Marker", lambda: str(M.Marker("python_version >= '3.8'")))
