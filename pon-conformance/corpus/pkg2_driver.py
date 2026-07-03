import pkg2
print(pkg2.pkg2_shadow.__name__)
print(pkg2.pkg2_shadow.__package__)
print(pkg2.__all__)
print(pkg2.base_tag)
print(pkg2.sib_tag)
print(pkg2.shadow_kind)
import pkg2_shadow
print(pkg2_shadow.__name__)
print(pkg2_shadow is pkg2.pkg2_shadow)
try:
    pkg2.leaky
    print("leaky-visible")
except AttributeError:
    print("leaky-hidden")
