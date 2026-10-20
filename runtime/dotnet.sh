#!/usr/bin/env bash
# The x86 .NET runtime the APK carries for the official PD2 launcher: Microsoft's two zip archives
# (config.sh pins them by SHA-512), unpacked together into work/runtime/dotnet (dotnet.exe,
# host/fxr, shared/Microsoft.NETCore.App and shared/Microsoft.WindowsDesktop.App). pack.py puts
# the tree at runner/dotnet-6.0.36-x86 in the managed root, which the prefix's C:\Program Files (x86)\dotnet links to.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if in_container; then
    python3 - "/cache/dl/$(basename "$DOTNET_RUNTIME_URL")" "/cache/dl/$(basename "$DOTNET_DESKTOP_URL")" <<'EOF'
import os, shutil, sys, zipfile
out = "/cache/work/runtime/dotnet"
shutil.rmtree(out, ignore_errors=True)
for archive in sys.argv[1:]:
    with zipfile.ZipFile(archive) as z:
        for info in z.infolist():
            name = os.path.normpath(info.filename)
            if name.startswith(("/", "..")):
                raise SystemExit(f"unsafe path in {archive}: {info.filename}")
            z.extract(info, out)
total = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(out) for f in fs)
print(f".NET: {total / 1e6:.1f} MB unpacked")
EOF
    exit 0
fi

fetch "$DOTNET_RUNTIME_URL" "$DOTNET_RUNTIME_SHA512" >/dev/null
fetch "$DOTNET_DESKTOP_URL" "$DOTNET_DESKTOP_SHA512" >/dev/null
run_in "$(runtime_image tools)" dotnet.sh
