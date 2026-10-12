# X server patches (Xvfb)

Applied by `build-xvfb.sh` to xorg-server 21.1.16 (`config.sh` pins the tarball).

- `0001-os-honour-nolock-for-every-uid.patch`: `-nolock` is honoured for every uid, not only root, so Xvfb creates no lock file in `/tmp`, which Android lacks.
- `0002-os-build-time-shell-path.patch`: the shell `System()` and `Popen()` run (XKB's `xkbcomp`) is the build-time macro `XSERVER_SHELL`, default `/bin/sh`; `build-xvfb.sh` sets `/system/bin/sh`.
- `0003-vfb-framebuffer-in-an-inherited-descriptor.patch`: the new `-fbfd <n>` puts screen 0's framebuffer, laid out as with `-fbdir`, in the inherited descriptor `n` (the app's memfd) instead of a file.
