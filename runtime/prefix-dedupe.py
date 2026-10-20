#!/usr/bin/env python3
"""prefix-dedupe.py <prefix> <full wine tree> <wine's place>: replace every file in the prefix that
is a byte copy of a file of the Wine tree (its modules, NLS files and fonts, matched by name) with a
relative link into the Wine tree, as the managed root lays it out: the prefix at <root>/prefix, the
Wine at <root>/<wine's place> (runner/wine-11.0-arm64). Wine opens some of them by path
(apisetschema.dll, locale.nls), so they must stay reachable there; a link to a file the reduced
Wine leaves out dangles, as if the file were missing. Prints the count and bytes saved."""
import filecmp
import os
import sys

prefix, wine, place = sys.argv[1:4]
# Where a copy can come from: the 64-bit modules first for system32 and its side-by-side folders.
A64 = "aarch64" if os.path.isdir(os.path.join(wine, "lib/wine/aarch64-windows")) else "x86_64"
SOURCES = [f"lib/wine/{A64}-windows", "lib/wine/i386-windows", "share/wine/nls", "share/wine/fonts"]
index = {}
for src in SOURCES:
    for n in os.listdir(os.path.join(wine, src)):
        index.setdefault(n.lower(), []).append(f"{src}/{n}")

linked = kept = 0
freed = 0
for root, dns, fns in os.walk(prefix):
    for n in fns:
        p = os.path.join(root, n)
        if os.path.islink(p):
            continue
        # The x86 CPU DLL is Box64's wowbox64.dll under Wine's default name.
        cands = index.get(n.lower(), [])
        if n.lower() == "xtajit.dll":
            cands = [f"lib/wine/{A64}-windows/wowbox64.dll"]
        for cand in cands:
            ref = os.path.join(wine, cand)
            if os.path.isfile(ref) and filecmp.cmp(p, ref, shallow=False):
                freed += os.path.getsize(p)
                os.unlink(p)
                # Relative to the file's folder: up to the managed root, then into the Wine.
                depth = os.path.relpath(root, prefix).count("/") + 2 if root != prefix else 1
                os.symlink("../" * depth + place + "/" + cand, p)
                linked += 1
                break
        else:
            kept += 1
print(f"linked {linked} copies ({freed / 1e6:.1f} MB), kept {kept} files")
