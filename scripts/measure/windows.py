#!/usr/bin/env python3
"""Is the app's window on screen, and is anything in front of it? (stdlib ctypes over CoreGraphics; no permissions)

  windows.py <pid> [<pid> ...]   prints one JSON line per window owned by those pids: on-screen flag, bounds,
                                  z-index among on-screen normal windows (0 = frontmost) and the owners of on-screen
                                  normal windows in front of it that overlap it.
"""
import ctypes
import ctypes.util
import json
import sys

cf = ctypes.CDLL(ctypes.util.find_library("CoreFoundation"))
cg = ctypes.CDLL(ctypes.util.find_library("CoreGraphics"))
cg.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
cg.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
cf.CFArrayGetCount.restype = ctypes.c_long
cf.CFArrayGetCount.argtypes = [ctypes.c_void_p]
cf.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
cf.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
cf.CFDictionaryGetValue.restype = ctypes.c_void_p
cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
cf.CFStringCreateWithCString.restype = ctypes.c_void_p
cf.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
cf.CFNumberGetValue.restype = ctypes.c_bool
cf.CFNumberGetValue.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p]
cf.CFBooleanGetValue.restype = ctypes.c_bool
cf.CFBooleanGetValue.argtypes = [ctypes.c_void_p]
cf.CFStringGetCString.restype = ctypes.c_bool
cf.CFStringGetCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_long, ctypes.c_uint32]
UTF8 = 0x08000100
ALL, ONSCREEN = 0, 1


def key(name):
    return cf.CFStringCreateWithCString(None, name.encode(), UTF8)


K = {k: key(k) for k in ("kCGWindowOwnerPID", "kCGWindowLayer", "kCGWindowIsOnscreen", "kCGWindowBounds",
                         "kCGWindowOwnerName", "X", "Y", "Width", "Height", "kCGWindowAlpha")}


def num(d, k, kind=4, ctype=ctypes.c_int32):
    v = cf.CFDictionaryGetValue(d, K[k])
    if not v:
        return None
    out = ctype()
    cf.CFNumberGetValue(v, kind, ctypes.byref(out))
    return out.value


def windows():
    arr = cg.CGWindowListCopyWindowInfo(ALL, 0)
    out = []
    for i in range(cf.CFArrayGetCount(arr)):
        d = cf.CFArrayGetValueAtIndex(arr, i)
        on = cf.CFDictionaryGetValue(d, K["kCGWindowIsOnscreen"])
        b = cf.CFDictionaryGetValue(d, K["kCGWindowBounds"])
        name = cf.CFDictionaryGetValue(d, K["kCGWindowOwnerName"])
        buf = ctypes.create_string_buffer(256)
        if name:
            cf.CFStringGetCString(name, buf, 256, UTF8)
        out.append({"pid": num(d, "kCGWindowOwnerPID"), "layer": num(d, "kCGWindowLayer"),
                    "onscreen": bool(on and cf.CFBooleanGetValue(on)), "owner": buf.value.decode(errors="replace"),
                    "alpha": num(d, "kCGWindowAlpha", 13, ctypes.c_double),
                    "bounds": [num(b, k, 13, ctypes.c_double) for k in ("X", "Y", "Width", "Height")]})
    return out  # front to back


def overlap(a, b):
    return a[0] < b[0] + b[2] and b[0] < a[0] + a[2] and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]


def main():
    pids = {int(p) for p in sys.argv[1:]}
    ws = [w for w in windows() if w["layer"] == 0 and w["bounds"][2] and w["bounds"][2] > 50]
    on = [w for w in ws if w["onscreen"] and (w["alpha"] or 0) > 0]
    for w in ws:
        if w["pid"] in pids:
            z = next((i for i, x in enumerate(on) if x is w), None)
            front = [x["owner"] for x in on[: z or 0] if overlap(x["bounds"], w["bounds"])] if z is not None else None
            print(json.dumps({"pid": w["pid"], "onscreen": w["onscreen"], "bounds": w["bounds"], "z": z,
                              "overlapping_in_front": front}))


if __name__ == "__main__":
    main()
