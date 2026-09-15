#!/usr/bin/env python3
"""Log X11 ClientMessages sent to the root window (e.g. _NET_WM_STATE, WM_CHANGE_STATE)
plus _NET_WM_STATE / WM_STATE property changes on all toplevel windows.
Usage: DISPLAY=:0 python3 x11mon.py"""
import ctypes, ctypes.util, sys, time, struct
X = ctypes.CDLL(ctypes.util.find_library("X11"))
Display = ctypes.c_void_p; Window = ctypes.c_ulong; Atom = ctypes.c_ulong
X.XOpenDisplay.restype = Display; X.XOpenDisplay.argtypes = [ctypes.c_char_p]
X.XDefaultRootWindow.restype = Window; X.XDefaultRootWindow.argtypes = [Display]
X.XSelectInput.argtypes = [Display, Window, ctypes.c_long]
X.XNextEvent.argtypes = [Display, ctypes.c_void_p]
X.XPending.argtypes = [Display]; X.XPending.restype = ctypes.c_int
X.XGetAtomName.restype = ctypes.c_void_p; X.XGetAtomName.argtypes = [Display, Atom]
X.XFree.argtypes = [ctypes.c_void_p]
X.XInternAtom.restype = Atom; X.XInternAtom.argtypes = [Display, ctypes.c_char_p, ctypes.c_int]
X.XQueryTree.argtypes = [Display, Window, ctypes.POINTER(Window), ctypes.POINTER(Window), ctypes.POINTER(ctypes.POINTER(Window)), ctypes.POINTER(ctypes.c_uint)]
X.XFetchName.argtypes = [Display, Window, ctypes.POINTER(ctypes.c_char_p)]; X.XFetchName.restype = ctypes.c_int
X.XGetWindowProperty.argtypes = [Display, Window, Atom, ctypes.c_long, ctypes.c_long, ctypes.c_int, Atom, ctypes.POINTER(Atom), ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_void_p)]
X.XGetWindowProperty.restype = ctypes.c_int
X.XSetErrorHandler.argtypes = [ctypes.c_void_p]
XRR = ctypes.CDLL(ctypes.util.find_library("Xrandr"))
XRR.XRRSelectInput.argtypes = [Display, Window, ctypes.c_int]
XRR.XRRQueryExtension.argtypes = [Display, ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int)]
XRR.XRRUpdateConfiguration.argtypes = [ctypes.c_void_p]
ERRH = ctypes.CFUNCTYPE(ctypes.c_int, Display, ctypes.c_void_p)
@ERRH
def _err(d, e): return 0
X.XSetErrorHandler(_err)
SubstructureNotifyMask = 1 << 19; SubstructureRedirectMask = 1 << 20; PropertyChangeMask = 1 << 22; FocusChangeMask = 1 << 21
ClientMessage = 33; PropertyNotify = 28; MapNotify = 19; UnmapNotify = 18; CreateNotify = 16; ConfigureNotify = 22; DestroyNotify = 17; FocusIn = 9; FocusOut = 10
class XEvent(ctypes.Union):
    _fields_ = [("type", ctypes.c_int), ("pad", ctypes.c_long * 24)]
d = X.XOpenDisplay(None)
if not d: sys.exit("cannot open display")
root = X.XDefaultRootWindow(d)
X.XSelectInput(d, root, SubstructureNotifyMask | PropertyChangeMask)
rr_base = ctypes.c_int(); rr_err = ctypes.c_int()
have_rr = bool(XRR.XRRQueryExtension(d, ctypes.byref(rr_base), ctypes.byref(rr_err)))
if have_rr: XRR.XRRSelectInput(d, root, 1 | 2)  # RRScreenChangeNotifyMask | RRCrtcChangeNotifyMask
def atom_name(a):
    p = X.XGetAtomName(d, a)
    if not p: return f"atom#{a}"
    s = ctypes.string_at(p).decode(); X.XFree(p); return s
def wname(w):
    n = ctypes.c_char_p()
    if X.XFetchName(d, w, ctypes.byref(n)) and n.value:
        s = n.value.decode(errors="replace"); X.XFree(n); return s
    return ""
def prop_atoms(w, name):
    a = X.XInternAtom(d, name.encode(), 0)
    t = Atom(); f = ctypes.c_int(); n = ctypes.c_ulong(); b = ctypes.c_ulong(); p = ctypes.c_void_p()
    if X.XGetWindowProperty(d, w, a, 0, 32, 0, 0, ctypes.byref(t), ctypes.byref(f), ctypes.byref(n), ctypes.byref(b), ctypes.byref(p)) != 0 or not p.value:
        return None
    vals = (ctypes.c_ulong * n.value).from_address(p.value)
    out = [atom_name(v) if f.value == 32 and t.value == 4 else str(v) for v in vals]
    X.XFree(p); return out
def geom(w):
    return prop_atoms(w, "_NET_WM_STATE")
watched = set()
def watch(w):
    if w in watched: return
    watched.add(w); X.XSelectInput(d, w, PropertyChangeMask | FocusChangeMask)
# watch existing toplevels
rr = Window(); pr = Window(); ch = ctypes.POINTER(Window)(); nc = ctypes.c_uint()
if X.XQueryTree(d, root, ctypes.byref(rr), ctypes.byref(pr), ctypes.byref(ch), ctypes.byref(nc)):
    for i in range(nc.value): watch(ch[i])
    X.XFree(ch)
t0 = time.time()
def stamp(): return f"[{time.time()-t0:8.3f}s]"
print(stamp(), "monitoring root", hex(root), "existing toplevels", len(watched), flush=True)
ev = XEvent()
while True:
    X.XNextEvent(d, ctypes.byref(ev))
    raw = bytes(ev)
    if ev.type == ClientMessage:
        # XClientMessageEvent: type,serial,send_event,display,window,message_type,format,data[20 bytes as 5 longs]
        _, serial, send_event, disp, win, mtype, fmt = struct.unpack_from("iLiPLLi", raw, 0)
        off = struct.calcsize("iLiPLLi"); off = (off + 7) & ~7
        data = struct.unpack_from("5l", raw, off)
        name = atom_name(mtype)
        extra = ""
        if name == "_NET_WM_STATE":
            act = {0: "REMOVE", 1: "ADD", 2: "TOGGLE"}.get(data[0], str(data[0]))
            extra = f" {act} {atom_name(data[1]) if data[1] else '-'} {atom_name(data[2]) if data[2] else '-'} src={data[3]}"
        elif name == "WM_CHANGE_STATE":
            extra = f" state={ {1:'Normal',3:'Iconic'}.get(data[0], data[0]) }"
        else:
            extra = f" data={data}"
        print(stamp(), f"ClientMessage {name} win={hex(win)} '{wname(win)}'{extra}", flush=True)
    elif ev.type == PropertyNotify:
        _, serial, send_event, disp, win, atom, tm, state = struct.unpack_from("iLiPLLLi", raw, 0)
        name = atom_name(atom)
        if name in ("_NET_WM_STATE", "WM_STATE", "_NET_WM_DESKTOP"):
            print(stamp(), f"PropertyNotify {name} win={hex(win)} '{wname(win)}' -> {prop_atoms(win, name)}", flush=True)
    elif ev.type in (MapNotify, UnmapNotify, CreateNotify, DestroyNotify):
        _, serial, send_event, disp, evwin, win = struct.unpack_from("iLiPLL", raw, 0)
        kind = {MapNotify: "Map", UnmapNotify: "Unmap", CreateNotify: "Create", DestroyNotify: "Destroy"}[ev.type]
        if ev.type == CreateNotify: watch(win)
        print(stamp(), f"{kind}Notify win={hex(win)} '{wname(win)}'", flush=True)
    elif have_rr and ev.type == rr_base.value:
        # XRRScreenChangeNotifyEvent: type,serial,send_event,display,window,root,timestamp,config_timestamp,size_index,subpixel,rotation,width,height,mwidth,mheight
        _, serial, send_event, disp, win, rootw, ts, cts, size_index, subpixel, rotation, w, h = struct.unpack_from("iLiPLLLLiiiii", raw, 0)
        XRR.XRRUpdateConfiguration(ctypes.byref(ev))
        print(stamp(), f"RRScreenChangeNotify screen={w}x{h}", flush=True)
    elif have_rr and ev.type == rr_base.value + 1:
        # XRRNotifyEvent subtype 0 = crtc change: type,serial,send_event,display,window,subtype, then XRRCrtcChangeNotifyEvent: timestamp,crtc,mode,rotation,x,y,width,height
        _, serial, send_event, disp, win, subtype = struct.unpack_from("iLiPLi", raw, 0)
        if subtype == 0:
            off = struct.calcsize("iLiPLi"); off = (off + 7) & ~7
            ts, crtc, mode, rotation, x, y, w, h = struct.unpack_from("LLLHiiII", raw, off)
            print(stamp(), f"RRCrtcChangeNotify crtc={crtc} mode={mode} {w}x{h}+{x}+{y}", flush=True)
    elif ev.type in (FocusIn, FocusOut):
        _, serial, send_event, disp, win, mode, detail = struct.unpack_from("iLiPLii", raw, 0)
        kind = "FocusIn" if ev.type == FocusIn else "FocusOut"
        print(stamp(), f"{kind} win={hex(win)} '{wname(win)}' mode={mode} detail={detail}", flush=True)
    elif ev.type == ConfigureNotify:
        _, serial, send_event, disp, evwin, win, x, y, w, h = struct.unpack_from("iLiPLLiiii", raw, 0)
        print(stamp(), f"ConfigureNotify win={hex(win)} '{wname(win)}' {w}x{h}+{x}+{y}", flush=True)
