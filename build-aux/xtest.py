"""Drive the pointer on a headless X display through XTEST, for the drills that need a real
press and drag (`ACCENT_BENCH_DIAGRAM=hold:…` prints where to aim).

Usage: build-aux/xtest.py :99 "drag 400 300 550 340; sleep 0.5; move 10 10; down; up"

Steps are `;`-separated: `move x y`, `down`, `up`, `drag x0 y0 x1 y1` (a press, twenty steps of
motion and a release) and `sleep s`. Coordinates are the screen's; with no window manager under
Xvfb a window sits at 0,0, so they are the window's too. Needs only libX11 and libXtst.
"""
import ctypes, sys, time
x11 = ctypes.cdll.LoadLibrary("libX11.so.6"); xt = ctypes.cdll.LoadLibrary("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
dpy = x11.XOpenDisplay(sys.argv[1].encode())
assert dpy, "no display"
def move(x, y):
    xt.XTestFakeMotionEvent(ctypes.c_void_p(dpy), -1, int(x), int(y), 0); x11.XFlush(ctypes.c_void_p(dpy))
def button(down, b=1):
    xt.XTestFakeButtonEvent(ctypes.c_void_p(dpy), b, 1 if down else 0, 0); x11.XFlush(ctypes.c_void_p(dpy))
for line in sys.argv[2].split(";"):
    w = line.split()
    if not w: continue
    if w[0] == "move": move(float(w[1]), float(w[2]))
    elif w[0] == "down": button(True)
    elif w[0] == "up": button(False)
    elif w[0] == "sleep": time.sleep(float(w[1]))
    elif w[0] == "drag":
        x0, y0, x1, y1 = map(float, w[1:5])
        move(x0, y0); time.sleep(0.1); button(True); time.sleep(0.1)
        for i in range(1, 21):
            move(x0 + (x1 - x0) * i / 20, y0 + (y1 - y0) * i / 20); time.sleep(0.02)
        time.sleep(0.1); button(False); time.sleep(0.2)
