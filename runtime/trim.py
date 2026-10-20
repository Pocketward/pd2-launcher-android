#!/usr/bin/env python3
"""trim.py <full wine tree> <keep list> <out tree>: the reduced Wine of the APK.

Keeps the loader, wineserver, the modules of the keep list on the PE sides its sections name
([both], [i386], [x86_64]: the 64-bit side) with their Unix libraries, wine.inf, Wine's fonts and
the NLS files without the CJK code pages.
Prints the sizes per part."""
import os
import shutil
import sys

full, keep_list, out = sys.argv[1:4]
# The 64-bit side: aarch64 for the ARM64 Wine (x86_64 for an x86-64 Wine).
A64 = "aarch64" if os.path.isdir(os.path.join(full, "lib/wine/aarch64-windows")) else "x86_64"
SIDES = {"both": ("i386-windows", f"{A64}-windows"), "i386": ("i386-windows",), "x86_64": (f"{A64}-windows",)}
names = []  # (name, sides)
section = "both"
for line in open(keep_list):
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    if line.startswith("["):
        section = line.strip("[]")
        continue
    names.append((line.lower(), SIDES[section]))
if os.path.exists(out):
    shutil.rmtree(out)


def cp(rel):
    src = os.path.join(full, rel)
    dst = os.path.join(out, rel)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.islink(src):
        os.symlink(os.readlink(src), dst)
    elif os.path.isdir(src):
        shutil.copytree(src, dst, symlinks=True)
    else:
        shutil.copy2(src, dst)


for rel in ["bin/wine", "bin/wineserver", f"lib/wine/{A64}-unix/wine", f"lib/wine/{A64}-unix/wine-preloader",
            "lib/x86_64-linux-gnu/libgcc_s.so.1", "share/wine/wine.inf", "share/wine/fonts"]:
    if os.path.lexists(os.path.join(full, rel)):
        cp(rel)
missing = []
for n, sides in names:
    found = False
    for side in sides:
        rel = f"lib/wine/{side}/{n}"
        if os.path.exists(os.path.join(full, rel)):
            cp(rel)
            found = True
    so = f"lib/wine/{A64}-unix/{n.rsplit('.', 1)[0]}.so"
    if os.path.exists(os.path.join(full, so)):
        cp(so)
    if not found:
        missing.append(n)
# NLS: all but the CJK double-byte code pages (932, 936, 949, 950 and their EUC/ISO forms).
nls = os.path.join(full, "share/wine/nls")
for n in sorted(os.listdir(nls)):
    if n in ("c_932.nls", "c_936.nls", "c_949.nls", "c_950.nls", "c_1361.nls", "c_20932.nls", "c_20949.nls",
             "c_10001.nls", "c_10002.nls", "c_10003.nls", "c_10008.nls"):
        continue
    cp(f"share/wine/nls/{n}")
if missing:
    print("not in this Wine:", " ".join(missing))


def size(p):
    t = 0
    for dp, dns, fns in os.walk(p):
        for f in fns:
            fp = os.path.join(dp, f)
            if not os.path.islink(fp):
                t += os.path.getsize(fp)
    return t


for part in ["bin", "lib/wine/i386-windows", f"lib/wine/{A64}-windows", f"lib/wine/{A64}-unix", "share/wine", "."]:
    print(f"{size(os.path.join(out, part)) / 1e6:8.1f} MB  {part}")
