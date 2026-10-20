#!/usr/bin/env python3
"""stage-wineb.py <ARM64 Wine tree> <wowbox64.dll> <out tree> <interpreter> [--xtajit]

Makes the ARM64 Wine runnable from the native userland: the ELF interpreter of Wine's programs
(the loader and wineserver) becomes <interpreter> (our glibc's loader below the data-folder
placeholder); the preloader's symbols are stripped (aarch64-linux-gnu-strip, or $STRIP); bin/wine links to
the loader as Wine installs it; wowbox64.dll goes into lib/wine/aarch64-windows, where Wine's
setup copies it into system32. With --xtajit, lib/wine/aarch64-windows/xtajit.dll (the name
Wine's WoW64 loads the x86 CPU DLL by) links to it too: for the build of the prefix, whose
system32 then holds xtajit.dll."""
import os
import shutil
import subprocess
import sys

src, wowbox, out, interp = sys.argv[1:5]
if os.path.exists(out):
    shutil.rmtree(out)
shutil.copytree(src, out, symlinks=True)
for rel in ["lib/wine/aarch64-unix/wine", "bin/wineserver"]:
    subprocess.run(["patchelf", "--set-interpreter", interp, os.path.join(out, rel)], check=True)
subprocess.run([os.environ.get("STRIP", "aarch64-linux-gnu-strip"), os.path.join(out, "lib/wine/aarch64-unix/wine-preloader")],
               check=True)
wine = os.path.join(out, "bin/wine")
if not os.path.lexists(wine):
    os.symlink("../lib/wine/aarch64-unix/wine", wine)
shutil.copy2(wowbox, os.path.join(out, "lib/wine/aarch64-windows/wowbox64.dll"))
if "--xtajit" in sys.argv[5:]:
    os.symlink("wowbox64.dll", os.path.join(out, "lib/wine/aarch64-windows/xtajit.dll"))
