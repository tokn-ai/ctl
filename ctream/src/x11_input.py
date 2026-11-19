"""X11 input endpoint. Run on the selected display with commands on stdin."""
import ctypes as c
import sys


def bind(lib, name, result, *args):
  function = getattr(lib, name)
  function.restype = result
  function.argtypes = args
  return function


x11 = c.CDLL("libX11.so.6")
ptr, num, ulong = c.c_void_p, c.c_int, c.c_ulong
display = bind(x11, "XOpenDisplay", ptr, c.c_char_p)(None)
if not display:
  sys.exit("Cannot open selected X11 display")
screen = bind(x11, "XDefaultScreen", num, ptr)(display)
width = bind(x11, "XDisplayWidth", num, ptr, num)(display, screen)
height = bind(x11, "XDisplayHeight", num, ptr, num)(display, screen)
if sys.argv[1:] == ["--geometry"]:
  print(width, height)
  bind(x11, "XCloseDisplay", num, ptr)(display)
  sys.exit(0)
xtst = c.CDLL("libXtst.so.6")
extension = bind(xtst, "XTestQueryExtension", num, ptr, *([c.POINTER(num)] * 4))
if not extension(display, *(c.byref(num()) for _ in range(4))):
  sys.exit("Selected display does not support XTEST")
motion = bind(xtst, "XTestFakeMotionEvent", num, ptr, num, num, num, ulong)
button = bind(xtst, "XTestFakeButtonEvent", num, ptr, c.c_uint, num, ulong)
key = bind(xtst, "XTestFakeKeyEvent", num, ptr, c.c_uint, num, ulong)
keysym = bind(x11, "XStringToKeysym", ulong, c.c_char_p)
keycode = bind(x11, "XKeysymToKeycode", c.c_uint8, ptr, ulong)
flush = bind(x11, "XFlush", num, ptr)
held = set()


def release():
  for kind, code in held:
    (key if kind == "key" else button)(display, code, 0, 0)
  held.clear()
  flush(display)


print(width, height, flush=True)
try:
  for line in sys.stdin:
    op, *args = line.split()
    if op == "reset":
      release()
    elif op == "move":
      x, y = map(int, args)
      motion(display, screen, x, y, 0)
    elif op in ("button", "key"):
      name, state = args
      code = int(name) if op == "button" else keycode(display, keysym(name.encode()))
      pressed = int(state)
      if code and bool(pressed) != ((op, code) in held):
        (button if op == "button" else key)(display, code, pressed, 0)
        if pressed:
          held.add((op, code))
        else:
          held.remove((op, code))
    flush(display)
finally:
  release()
  bind(x11, "XCloseDisplay", num, ptr)(display)
