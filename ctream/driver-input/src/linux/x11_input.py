"""X11 input endpoint. Run on the selected display with commands on stdin."""
import ctypes as c
import os
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
sync = bind(x11, "XSync", num, ptr, num)
synchronized = sys.argv[1:] == ["--sync"]
held = set()
server_error = False

if synchronized:
  # Xlib's default handler exits the process on asynchronous errors, bypassing
  # the release of held inputs. XSync delivers pending errors before the ack.
  handler = c.CFUNCTYPE(num, ptr, ptr)

  @handler
  def record_error(_display, _event):
    global server_error
    server_error = True
    return 0

  bind(x11, "XSetErrorHandler", ptr, handler)(record_error)


def release():
  for kind, code in list(held):
    sent = (key if kind == "key" else button)(display, code, 0, 0)
    if sent or not synchronized:
      held.remove((kind, code))
  flush(display)
  if held:
    raise RuntimeError("failed to release X11 input")


print(width, height, flush=True)
try:
  for line in sys.stdin:
    op, *args = line.split()
    if op == "sync" and synchronized and not args:
      sync(display, 0)
      if server_error:
        raise RuntimeError("X server rejected an input request")
      print("ok", flush=True)
    elif op == "reset" and (not synchronized or not args):
      release()
    elif op == "move":
      x, y = map(int, args)
      if synchronized and not (0 <= x < width and 0 <= y < height):
        raise ValueError("X11 pointer is outside display")
      sent = motion(display, screen, x, y, 0)
      if synchronized and not sent:
        raise RuntimeError("XTEST rejected pointer motion")
    elif op in ("button", "key"):
      name, state = args
      if op == "button":
        code = int(name)
      else:
        symbol = keysym(name.encode())
        code = keycode(display, symbol) if symbol or not synchronized else 0
      pressed = int(state)
      if synchronized and (not code or state not in ("0", "1") or (op == "button" and not 1 <= code <= 9)):
        raise ValueError("unsupported X11 key or button event")
      if code and bool(pressed) != ((op, code) in held):
        sent = (button if op == "button" else key)(display, code, pressed, 0)
        if synchronized and not sent:
          raise RuntimeError("XTEST rejected input event")
        if pressed:
          held.add((op, code))
        else:
          held.remove((op, code))
    elif synchronized:
      raise ValueError("unknown X11 input command")
    flush(display)
except (ValueError, RuntimeError):
  if synchronized and os.environ.get("CTREAM_INPUT_WORKER") == "1":
    print("refused", flush=True)
  raise
finally:
  try:
    release()
  finally:
    bind(x11, "XCloseDisplay", num, ptr)(display)
