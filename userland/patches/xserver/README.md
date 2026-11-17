# X server patches (Xvfb)

Applied by `build-xvfb.sh` to xorg-server 21.1.25 (`config.sh` pins the tarball), each exactly (`--fuzz=0`).

- `0001-os-honour-nolock-for-every-uid.patch`: `-nolock` is honoured for every uid, not only root, so Xvfb creates no lock file in `/tmp`, which Android lacks.
- `0002-os-build-time-shell-path.patch`: the shell `System()` and `Popen()` run (XKB's `xkbcomp`) is the build-time macro `XSERVER_SHELL`, default `/bin/sh`; `build-xvfb.sh` sets `/system/bin/sh`.
- `0003-vfb-framebuffer-in-an-inherited-descriptor.patch`: the new `-fbfd <n>` puts screen 0's framebuffer, laid out as with `-fbdir`, in the inherited descriptor `n` (the app's memfd) instead of a file.
- `0004-vfb-listen-on-an-inherited-socket.patch`: the new `-listenfd <n>` makes the inherited, listening socket `n` the server's only listener (the server's own `ListenOnOpenFD`, with `NoListenAll`): the app binds the socket to a file in its private runtime folder, and clients connect with `DISPLAY` set to that path. Without it Xvfb listens on the abstract socket `@/tmp/.X11-unix/X<n>`, which every app on the device shares: another app's X server on the same number stopped ours ("server already running", issue #26).
