from .base import *
from .pkg2_shadow import *

__all__ = base.__all__ + pkg2_shadow.__all__
shadow_kind = pkg2_shadow.__name__
