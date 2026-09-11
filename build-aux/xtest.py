"""Drive the pointer on a headless X display through XTEST, for the drills that need a real
press and drag (`ACCENT_BENCH_DIAGRAM=hold:…` prints where to aim).

Usage: build-aux/xtest.py :99 "drag 400 300 550 340; sleep 0.5; move 10 10; down; up"

Steps are `;`-separated: `move x y`, `down`, `up`, `drag x0 y0 x1 y1` (a press, twenty steps of
motion and a release), `sleep s`, `focus` (give the keyboard to the window under the pointer,
which no window manager does under Xvfb), `key <chord>` (keysym names joined by `+`, e.g.
`ctrl+Return`) and `type <text>` (letters, digits and spaces). Coordinates are the screen's; with
no window manager under Xvfb a window sits at 0,0, so they are the window's too. Needs only libX11
and libXtst.
"""
import ctypes, sys, time
x11 = ctypes.cdll.LoadLibrary("libX11.so.6"); xt = ctypes.cdll.LoadLibrary("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XDefaultRootWindow.restype = ctypes.c_ulong
dpy = x11.XOpenDisplay(sys.argv[1].encode())
assert dpy, "no display"
def move(x, y):
    xt.XTestFakeMotionEvent(ctypes.c_void_p(dpy), -1, int(x), int(y), 0); x11.XFlush(ctypes.c_void_p(dpy))
def keycode(name):
    name = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L", " ": "space"}.get(name, name)
    sym = x11.XStringToKeysym(name.encode())
    assert sym, f"no keysym {name}"
    return x11.XKeysymToKeycode(ctypes.c_void_p(dpy), ctypes.c_ulong(sym))
def key(chord):
    codes = [keycode(n) for n in chord.split("+")]
    for c in codes: xt.XTestFakeKeyEvent(ctypes.c_void_p(dpy), c, 1, 0)
    for c in reversed(codes): xt.XTestFakeKeyEvent(ctypes.c_void_p(dpy), c, 0, 0)
    x11.XFlush(ctypes.c_void_p(dpy)); time.sleep(0.03)
def focus():
    root, child = ctypes.c_ulong(), ctypes.c_ulong()
    i = ctypes.c_int()
    u = ctypes.c_uint()
    x11.XQueryPointer(ctypes.c_void_p(dpy), ctypes.c_ulong(x11.XDefaultRootWindow(ctypes.c_void_p(dpy))),
                      ctypes.byref(root), ctypes.byref(child), ctypes.byref(i), ctypes.byref(i),
                      ctypes.byref(i), ctypes.byref(i), ctypes.byref(u))
    assert child.value, "no window under the pointer"
    x11.XSetInputFocus(ctypes.c_void_p(dpy), child, 1, 0); x11.XFlush(ctypes.c_void_p(dpy))
def button(down, b=1):
    xt.XTestFakeButtonEvent(ctypes.c_void_p(dpy), b, 1 if down else 0, 0); x11.XFlush(ctypes.c_void_p(dpy))
for line in sys.argv[2].split(";"):
    w = line.split()
    if not w: continue
    if w[0] == "move": move(float(w[1]), float(w[2]))
    elif w[0] == "down": button(True)
    elif w[0] == "up": button(False)
    elif w[0] == "sleep": time.sleep(float(w[1]))
    elif w[0] == "focus": focus()
    elif w[0] == "key": key(w[1])
    elif w[0] == "type":
        for ch in line.split(None, 1)[1]:
            key(f"shift+{ch}" if ch.isupper() else ch)
    elif w[0] == "drag":
        x0, y0, x1, y1 = map(float, w[1:5])
        move(x0, y0); time.sleep(0.1); button(True); time.sleep(0.1)
        for i in range(1, 21):
            move(x0 + (x1 - x0) * i / 20, y0 + (y1 - y0) * i / 20); time.sleep(0.02)
        time.sleep(0.1); button(False); time.sleep(0.2)
