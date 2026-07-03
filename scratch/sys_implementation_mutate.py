import sys
orig = sys.implementation.cache_tag
sys.implementation.cache_tag = None
print(sys.implementation.cache_tag is None)
sys.implementation.cache_tag = orig
print(sys.implementation.cache_tag)
