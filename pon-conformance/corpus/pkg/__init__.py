root_name = __name__
root_package = __package__
x = "pkg-x"
from . import sib
from .sib import x as sib_x
init_result = root_name + "|" + root_package + "|" + sib.x + "|" + sib_x
