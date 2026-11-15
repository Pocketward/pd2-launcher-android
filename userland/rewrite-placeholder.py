#!/usr/bin/env python3
"""rewrite-placeholder.py <tree> <data> <manifest>...: rewrites the unpacked App userland <tree> for
the data folder <data> as the app rewrites it (UserlandInstaller): each copy of the placeholder that
a manifest lists for a file (the main manifest and those of the parts unpacked over it) becomes the
real path, padded with '/' to the placeholder's length in a binary file, exactly in a text file.
Then no file of the tree may hold the placeholder's first part. check-tree.sh and
check-relocated.sh run it."""
import json
import os
import sys

tree, data, *manifests = sys.argv[1:]
if not manifests:
    sys.exit("usage: rewrite-placeholder.py <tree> <data> <manifest>...")
entries, placeholder = [], None
for manifest in manifests:
    m = json.load(open(manifest))
    if placeholder not in (None, m["placeholder"]):
        sys.exit(f"FAIL: {manifest} names another placeholder")
    placeholder = m["placeholder"]
    entries += m["entries"]
P = placeholder.encode()
exact = data.encode()
if len(exact) > len(P):
    sys.exit(f"FAIL: the data folder {data} is longer than the placeholder")
padded = exact + b"/" * (len(P) - len(exact))
for e in entries:
    if not e.get("placeholders"):
        continue
    path = os.path.join(tree, e["path"])
    old = open(path, "rb").read()
    new, at = bytearray(), 0
    for o in e["placeholders"]:
        assert old[o:o + len(P)] == P, e["path"]
        new += old[at:o] + (exact if e.get("text") else padded)
        at = o + len(P)
    new += old[at:]
    mode = os.stat(path).st_mode
    os.chmod(path, mode | 0o200)
    open(path, "wb").write(new)
    os.chmod(path, mode)
left = []
for dirpath, _, files in os.walk(tree):
    for n in files:
        f = os.path.join(dirpath, n)
        if not os.path.islink(f) and P[:32] in open(f, "rb").read():
            left.append(os.path.relpath(f, tree))
if left:
    sys.exit(f"FAIL: the placeholder is left in {left}")
print(f"rewritten for {data}: no placeholder left")
