# _colorize.ANSIColors escape-code table and get_colors(): the surface
# doctest binds at import (`from _colorize import ANSIColors, can_colorize`)
# and calls when summarizing runs.  Output is piped in both runners, and
# they share one environment, so the can_colorize ladder answers alike.
import _colorize
from _colorize import ANSIColors, can_colorize

print(repr(ANSIColors.RED), repr(ANSIColors.RESET))
print(repr(ANSIColors.BOLD_GREEN), repr(ANSIColors.GREY), repr(ANSIColors.INTENSE_BACKGROUND_YELLOW))
c = _colorize.get_colors()
print(repr(c.BOLD_GREEN), repr(c.RESET))
f = _colorize.get_colors(True)
print(repr(f.BOLD_GREEN), repr(f.RESET))
k = _colorize.get_colors(colorize=True)
print(repr(k.RED))
n = _colorize.get_colors(False)
print(repr(n.BACKGROUND_MAGENTA))
print(_colorize.COLORIZE)
