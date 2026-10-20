#!/usr/bin/env python3
"""pack.py <wine tree> <prefix template> <dotnet tree> <cut list> <out dir>: the app's runtime asset.

Writes <out>/runtime.tar.zst (GNU tar, zstd -19) and <out>/runtime.info (line 1: the version, the
first 16 hex digits of a SHA-256 over the contents; line 2: the tar's size in bytes, for the
unpack's progress). The paths are the Launcher's managed root's, as the engine names them
(src/runtime.rs):
  relocate.list                    first: one "binary <path>" line for each file that holds the
                                   data-folder placeholder (the ELF interpreter of Wine's
                                   programs), which the app rewrites as it unpacks the file
  runner/wine-11.0-arm64/          the reduced Wine, bin/wine a copy of its loader (the engine
                                   opens bin/wine and follows no link), with links to the
                                   loader's ntdll.so and wine-preloader beside it (the loader
                                   looks for them in its own folder)
  runner/wine-11.0-arm64-prefix/   the prefix template
  runner/dotnet-6.0.36-x86/        the official PD2 launcher's x86 .NET runtime
without what the cut list names (wine/... and dotnet/... paths, a folder with all it holds).
Prints the sizes."""
import hashlib
import io
import os
import subprocess
import sys
import tarfile

wine, prefix, dotnet, cut_list, out = sys.argv[1:6]
WINE = "runner/wine-11.0-arm64"
PREFIX = "runner/wine-11.0-arm64-prefix"
DOTNET = "runner/dotnet-6.0.36-x86"
PLACEHOLDER = b"/pd2launcher-data-placeholder-" + b"0" * 170
cuts = [l.strip() for l in open(cut_list) if l.strip() and not l.startswith("#")]
os.makedirs(out, exist_ok=True)
tar_path = os.path.join(out, "runtime.tar")
digest = hashlib.sha256()


def cut(rel):
    return any(rel == c or rel.startswith(c + "/") for c in cuts)


def walk(root, cut_root):
    """(path, its name below root) in sorted order, without the cut ones."""
    for dp, dns, fns in os.walk(root):
        dns.sort()
        dns[:] = [d for d in dns if not cut(f"{cut_root}/{os.path.relpath(os.path.join(dp, d), root)}")]
        for n in sorted(dns + fns):
            p = os.path.join(dp, n)
            rel = os.path.relpath(p, root)
            if cut_root and cut(f"{cut_root}/{rel}"):
                continue
            yield p, rel


def entry(tar, path, arc, data=None):
    ti = tar.gettarinfo(path, arc)
    ti.uid = ti.gid = 0
    ti.uname = ti.gname = ""
    ti.mtime = 0
    if data is not None:
        ti.type = tarfile.REGTYPE
        ti.linkname = ""
        ti.size = len(data)
        ti.mode = 0o755
    digest.update(arc.encode())
    digest.update(oct(ti.mode).encode())
    if ti.isreg():
        if data is None:
            data = open(path, "rb").read()
        digest.update(data)
        tar.addfile(ti, io.BytesIO(data))
        return len(data)
    if ti.issym() or ti.isdir():
        digest.update(ti.linkname.encode())
        tar.addfile(ti)
        return 0
    raise SystemExit(f"pack.py: {path} is not a file, a link or a folder")


# The files that hold the placeholder: bin/wine (a copy of the loader) too.
relocate = []
for p, rel in walk(wine, "wine"):
    if not os.path.islink(p) and os.path.isfile(p) and PLACEHOLDER in open(p, "rb").read():
        relocate.append(f"{WINE}/{rel}")
loader = os.path.join(wine, "lib/wine/aarch64-unix/wine")
if f"{WINE}/bin/wine" not in relocate:
    relocate.append(f"{WINE}/bin/wine")
listing = "".join(f"binary {p}\n" for p in sorted(relocate)).encode()
digest.update(listing)
sizes = {}
with tarfile.open(tar_path, "w", format=tarfile.GNU_FORMAT) as tar:
    ti = tarfile.TarInfo("relocate.list")
    ti.size = len(listing)
    ti.mode = 0o644
    tar.addfile(ti, io.BytesIO(listing))
    for name, root, arc_root, cut_root in (("wine", wine, WINE, "wine"), ("prefix", prefix, PREFIX, ""),
                                          ("dotnet", dotnet, DOTNET, "dotnet")):
        total = entry(tar, root, arc_root)
        for p, rel in walk(root, cut_root):
            arc = f"{arc_root}/{rel}"
            if arc == f"{WINE}/bin/wine":
                total += entry(tar, p, arc, data=open(loader, "rb").read())
                for beside in ("ntdll.so", "wine-preloader"):
                    ti = tarfile.TarInfo(f"{WINE}/bin/{beside}")
                    ti.type = tarfile.SYMTYPE
                    ti.linkname = f"../lib/wine/aarch64-unix/{beside}"
                    ti.mode = 0o777
                    digest.update(ti.name.encode() + ti.linkname.encode())
                    tar.addfile(ti)
            else:
                total += entry(tar, p, arc)
        sizes[name] = total
subprocess.run(["zstd", "-19", "-T0", "-q", "-f", tar_path, "-o", tar_path + ".zst"], check=True)
tar_size = os.path.getsize(tar_path)
os.remove(tar_path)
with open(os.path.join(out, "runtime.info"), "w") as f:
    f.write(f"{digest.hexdigest()[:16]}\n{tar_size}\n")
for k, v in sizes.items():
    print(f"{k:8s} {v / 1e6:8.1f} MB unpacked")
print(f"relocated: {', '.join(sorted(relocate))}")
print(f"runtime.tar.zst {os.path.getsize(tar_path + '.zst') / 1e6:.1f} MB, tar {tar_size / 1e6:.1f} MB")
