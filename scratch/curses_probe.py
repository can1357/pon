import _curses, _curses_panel
print('_curses_ok', _curses.__name__)
for name in ['__version__','ncurses_version','erasechar','getsyx','setsyx','tparm','unctrl','ungetch','unget_wch','ungetmouse']:
    print('has', name, hasattr(_curses, name))
print('tparm', _curses.tparm(b'\x1b[%i%p1%d;%p2%dH', 0, 0))
for fn, arg in [(_curses.ungetch, ord('A')), (_curses.unget_wch, 'Z')]:
    try:
        print(fn.__name__, fn(arg))
    except Exception as e:
        print(fn.__name__, type(e).__name__, e)
try:
    _curses.erasechar()
except Exception as e:
    print('erasechar', type(e).__name__, e)
print('_curses_panel_ok', _curses_panel.__name__)
for name in ['__version__','top_panel','bottom_panel']:
    print('panel_has', name, hasattr(_curses_panel, name))
try:
    print('top_panel', _curses_panel.top_panel())
except Exception as e:
    print('top_panel', type(e).__name__, e)
import curses
import curses.panel
print('curses_ok', curses.__name__, hasattr(curses, 'erasechar'), hasattr(curses, 'tparm'))
print('panel_ok', curses.panel.__name__, hasattr(curses.panel, 'top_panel'))
