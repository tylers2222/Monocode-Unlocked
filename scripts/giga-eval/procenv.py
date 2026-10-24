#!/usr/bin/env python3
"""Print selected environment variables of a running process (same user).

    procenv.py <pid> NAME [NAME...]

Linux reads /proc/<pid>/environ. macOS reads KERN_PROCARGS2, because macOS
`ps` no longer shows another process's environment. Prints NAME=value lines
only for the names asked for.
"""
import ctypes
import ctypes.util
import struct
import sys

CTL_KERN, KERN_ARGMAX, KERN_PROCARGS2 = 1, 8, 49
libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)


def sysctl(mib, size):
    arr = (ctypes.c_int * len(mib))(*mib)
    buf = ctypes.create_string_buffer(size)
    length = ctypes.c_size_t(size)
    if libc.sysctl(arr, len(mib), buf, ctypes.byref(length), None, 0) != 0:
        raise OSError(ctypes.get_errno(), "sysctl failed")
    return buf.raw[: length.value]


def to_env(items):
    env = {}
    for item in items:
        if not item:
            break
        key, _, value = item.decode("utf-8", "replace").partition("=")
        env.setdefault(key, value)
    return env


def environ(pid):
    if sys.platform.startswith("linux"):
        with open(f"/proc/{pid}/environ", "rb") as f:
            return to_env(f.read().split(b"\0"))
    argmax = struct.unpack("i", sysctl([CTL_KERN, KERN_ARGMAX], 4))[0]
    raw = sysctl([CTL_KERN, KERN_PROCARGS2, pid], argmax)
    argc = struct.unpack("i", raw[:4])[0]
    rest = raw[4:]
    exe_end = rest.index(b"\0")
    rest = rest[exe_end:].lstrip(b"\0")
    return to_env(rest.split(b"\0")[argc:])


def main():
    pid, names = int(sys.argv[1]), sys.argv[2:]
    try:
        env = environ(pid)
    except OSError:
        sys.exit(1)
    found = False
    for name in names:
        if name in env:
            print(f"{name}={env[name]}")
            found = True
    sys.exit(0 if found else 1)


if __name__ == "__main__":
    main()
