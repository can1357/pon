# PEP 667 sys._getframe() probe surface: frame type identity and the
# distinctly-typed, process-stable FrameLocalsProxy behind function-frame
# f_locals reads (the shape _collections_abc snapshots for ABC registration).
import sys

def probe_locals_type():
    return type(sys._getframe().f_locals)

def probe_caller_frame_type():
    return type(sys._getframe(1))

print(type(sys._getframe()).__name__)
print(type(sys._getframe(0)).__name__)
print(type(sys._getframe(False)).__name__)
print(probe_caller_frame_type().__name__)
print(probe_locals_type().__name__)
print(probe_locals_type() is probe_locals_type())
print(isinstance(probe_locals_type(), type))
