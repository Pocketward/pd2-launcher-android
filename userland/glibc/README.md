# glibc for the App userland

Debian 13's glibc 2.41 (source package `2.41-12+deb13u4` from snapshot.debian.org `20261006T000000Z`; `build.sh` pins the SHA-256 sums), cross-compiled for aarch64 with Debian's configure options and multiarch layout below `<U>`, the userland prefix (`USERLAND` in `../config.sh`). Debian's patch series is applied first, then ours. Our patches stop glibc from issuing syscalls that the Android 11 (API 30) app seccomp policy traps, and make the resolver and NSS read their files from `<U>/etc`. Android 11 traps every syscall that Android 12, 12L and 13 trap, and six more (`bpf`, `close_range`, `pidfd_getfd`, `pidfd_send_signal`, `process_madvise`, `userfaultfd`), so one glibc serves every Android the app installs on (minSdk 30); those six answer ENOSYS on every release. The lists are in `tests/allowed-syscalls.txt` and `tests/blocked-syscalls.txt`.

## Build

```sh
userland/glibc/build.sh
```

It runs in a pinned `debian:trixie-slim` container (`Containerfile`), unpacks the source with `dpkg-source -x` (Debian's `debian/patches/series`), applies `patches/*.patch` with `patch -p1 --fuzz=0`, then configures, builds and installs into `out/install` (the contents of `<U>`).

## Patches

- `0001-rseq-default-off.patch`: the tunable `glibc.pthread.rseq` has default and maximum 0, so no thread registers rseq.
- `0002-no-set-robust-list.patch`: `set_robust_list` is never issued; glibc releases a dead thread's robust mutexes itself, and a robust process-shared or priority-inheritance mutex's init returns ENOTSUP.
- `0003-clone3-to-clone.patch`: `pthread_create` and `posix_spawn` use `clone`; `clone3` is never issued.
- `0004-faccessat2-fchmodat2-fallback.patch`: `faccessat` and `fchmodat` take glibc's existing fallbacks instead of `faccessat2`/`fchmodat2`; `faccessat` asks the kernel again for a file that is no symlink.
- `0005-accept-via-accept4.patch`: `accept()` is `accept4(fd, addr, len, 0)`.
- `0006-sysv-mq-enosys.patch`: SysV IPC and POSIX message queue functions fail with ENOSYS without a syscall.
- `0007-setxid-no-syscall.patch`: the set*id calls answer in user space (0 for a no-op, else EPERM); `setresuid` is unchanged.
- `0008-etc-paths-from-sysconfdir.patch`: the resolver and NSS read their files from sysconfdir (`<U>/etc`) instead of `/etc`.
- `0009-syscall-blocked-enosys.patch`: `syscall()` returns ENOSYS for any number outside the Android 11 app allowlist (Wine's `userfaultfd` among them: Wine then keeps its older write watches).
- `0010-blocked-wrappers-no-syscall.patch`: the remaining wrappers of blocked syscalls answer EPERM or ENOSYS without a syscall; reads of the NTP state succeed with an unsynchronised clock's values; `closefrom` and `posix_spawn`'s closefrom action close through `/proc/self/fd` instead of `close_range`.
