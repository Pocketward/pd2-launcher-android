// Debug probe of the app process's seccomp policy: which syscalls it lets through.
// Each syscall runs in a forked child with harmless arguments; the parent reports whether the
// child was killed by SIGSYS or what the call returned.
//
// Syscall numbers come from the NDK r27c <asm/unistd.h> (aarch64 uses asm-generic/unistd.h);
// every number used here is defined there.

#define _GNU_SOURCE // pipe2

#include <jni.h>

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <linux/kcmp.h>
#include <linux/keyctl.h>
#include <linux/landlock.h>
#include <linux/openat2.h>

#define OUT_SIZE 16384
#define CHILD_TIMEOUT_MS 2000

struct out {
    char buf[OUT_SIZE];
    size_t len;
};

static void append(struct out *o, const char *fmt, ...) {
    if (o->len >= sizeof(o->buf) - 1) return;
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(o->buf + o->len, sizeof(o->buf) - o->len, fmt, ap);
    va_end(ap);
    if (n > 0) {
        o->len += (size_t)n;
        if (o->len > sizeof(o->buf) - 1) o->len = sizeof(o->buf) - 1;
    }
}

#ifdef __aarch64__

// IPC keys that nobody creates; without IPC_CREAT the *get calls only look them up.
#define PROBE_IPC_KEY ((key_t)0x50443250)

static long do_openat2(void) {
    struct open_how how = {.flags = O_RDONLY | O_DIRECTORY | O_CLOEXEC};
    return syscall(__NR_openat2, AT_FDCWD, "/", &how, sizeof(how));
}
static long do_close_range(void) { return syscall(__NR_close_range, 1000, 1000, 0); }
static long do_clone3(void) { return syscall(__NR_clone3, NULL, 0); } // size 0: EINVAL, no child
static long do_faccessat2(void) { return syscall(__NR_faccessat2, AT_FDCWD, "/", F_OK, 0); }
static long do_fchmodat2(void) {
    return syscall(__NR_fchmodat2, AT_FDCWD, "/pd2-seccomp-probe-missing", 0644, 0);
}
static long do_rseq(void) { return syscall(__NR_rseq, NULL, 0, 0, 0); } // EINVAL if allowed
static long do_set_robust_list(void) { return syscall(__NR_set_robust_list, NULL, 0); } // EINVAL
static long do_get_robust_list(void) {
    void *head = NULL;
    size_t len = 0;
    return syscall(__NR_get_robust_list, 0, &head, &len);
}
static long do_accept(void) { return syscall(__NR_accept, -1, NULL, NULL); }
static long do_accept4(void) { return syscall(__NR_accept4, -1, NULL, NULL, 0); }
static long do_shmget(void) { return syscall(__NR_shmget, PROBE_IPC_KEY, 0, 0); }
static long do_semget(void) { return syscall(__NR_semget, PROBE_IPC_KEY, 0, 0); }
static long do_msgget(void) { return syscall(__NR_msgget, PROBE_IPC_KEY, 0); }
static long do_epoll_pwait2(void) {
    char events[16];
    return syscall(__NR_epoll_pwait2, -1, events, 1, NULL, NULL, 0);
}
static long do_futex_waitv(void) { return syscall(__NR_futex_waitv, NULL, 0, 0, NULL, 0); }
static long do_statx(void) {
    char buf[256];
    return syscall(__NR_statx, AT_FDCWD, "/", 0, 0x7ffU /* STATX_BASIC_STATS */, buf);
}
static long do_setuid(void) { return syscall(__NR_setuid, getuid()); }
static long do_setresgid(void) { return syscall(__NR_setresgid, -1, -1, -1); } // no change
static long do_setresuid(void) { return syscall(__NR_setresuid, -1, -1, -1); } // no change
static long do_kcmp(void) { return syscall(__NR_kcmp, getpid(), getpid(), KCMP_FILE, 0, 0); }
static long do_keyctl(void) {
    return syscall(__NR_keyctl, KEYCTL_GET_KEYRING_ID, KEY_SPEC_SESSION_KEYRING, 0);
}
static long do_landlock_create_ruleset(void) {
    return syscall(__NR_landlock_create_ruleset, NULL, 0, LANDLOCK_CREATE_RULESET_VERSION);
}
static long do_mq_open(void) {
    return syscall(__NR_mq_open, "pd2-seccomp-probe-missing", O_RDONLY, 0, NULL);
}
static long do_memfd_create(void) { return syscall(__NR_memfd_create, "pd2-probe", 0); }
static long do_pidfd_open(void) { return syscall(__NR_pidfd_open, getpid(), 0); }
static long do_openat(void) { return syscall(__NR_openat, AT_FDCWD, "/", O_RDONLY | O_DIRECTORY); }
static long do_getpid(void) { return syscall(__NR_getpid); }

struct probe {
    const char *name;
    long (*call)(void);
};

static const struct probe probes[] = {
    {"getpid (control, allowed)", do_getpid},
    {"openat (control, allowed)", do_openat},
    {"openat2", do_openat2},
    {"close_range", do_close_range},
    {"clone3", do_clone3},
    {"faccessat2", do_faccessat2},
    {"fchmodat2", do_fchmodat2},
    {"rseq", do_rseq},
    {"set_robust_list", do_set_robust_list},
    {"get_robust_list", do_get_robust_list},
    {"accept", do_accept},
    {"accept4", do_accept4},
    {"shmget", do_shmget},
    {"semget", do_semget},
    {"msgget", do_msgget},
    {"epoll_pwait2", do_epoll_pwait2},
    {"futex_waitv", do_futex_waitv},
    {"statx", do_statx},
    {"setuid", do_setuid},
    {"setresgid", do_setresgid},
    {"setresuid", do_setresuid},
    {"kcmp", do_kcmp},
    {"keyctl", do_keyctl},
    {"landlock_create_ruleset", do_landlock_create_ruleset},
    {"mq_open", do_mq_open},
    {"memfd_create", do_memfd_create},
    {"pidfd_open", do_pidfd_open},
};

struct child_result {
    long ret;
    int err;
};

static void run_probe(struct out *o, const struct probe *p) {
    int fds[2];
    if (pipe2(fds, O_CLOEXEC) != 0) {
        append(o, "%-26s probe error: pipe2 errno %d (%s)\n", p->name, errno, strerror(errno));
        return;
    }
    pid_t pid = fork();
    if (pid < 0) {
        append(o, "%-26s probe error: fork errno %d (%s)\n", p->name, errno, strerror(errno));
        close(fds[0]);
        close(fds[1]);
        return;
    }
    if (pid == 0) {
        // Default SIGSYS disposition so a trapped call kills the child at once (no debuggerd dump).
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_handler = SIG_DFL;
        sigaction(SIGSYS, &sa, NULL);
        sigset_t set;
        sigemptyset(&set);
        sigaddset(&set, SIGSYS);
        sigprocmask(SIG_UNBLOCK, &set, NULL);

        errno = 0;
        struct child_result r;
        r.ret = p->call();
        r.err = r.ret == -1 ? errno : 0;
        ssize_t unused = write(fds[1], &r, sizeof(r));
        (void)unused;
        _exit(0);
    }
    close(fds[1]);

    int status = 0;
    pid_t w = 0;
    for (int waited = 0; waited < CHILD_TIMEOUT_MS; waited += 10) {
        w = waitpid(pid, &status, WNOHANG);
        if (w != 0) break;
        struct timespec ts = {0, 10 * 1000 * 1000};
        nanosleep(&ts, NULL);
    }
    if (w == 0) {
        kill(pid, SIGKILL);
        waitpid(pid, &status, 0);
        append(o, "%-26s TIMEOUT (child killed after %d ms)\n", p->name, CHILD_TIMEOUT_MS);
        close(fds[0]);
        return;
    }

    struct child_result r;
    ssize_t n = read(fds[0], &r, sizeof(r));
    close(fds[0]);

    if (WIFSIGNALED(status)) {
        int sig = WTERMSIG(status);
        append(o, "%-26s %s (signal %d)\n", p->name, sig == SIGSYS ? "KILLED by SIGSYS" : "KILLED", sig);
    } else if (n == (ssize_t)sizeof(r)) {
        if (r.ret == -1)
            append(o, "%-26s returned -1 errno %d (%s)\n", p->name, r.err, strerror(r.err));
        else
            append(o, "%-26s returned %ld\n", p->name, r.ret);
    } else {
        append(o, "%-26s child exited (status 0x%x) without a result\n", p->name, status);
    }
}

#endif // __aarch64__

static void append_status_lines(struct out *o) {
    FILE *f = fopen("/proc/self/status", "re");
    if (!f) {
        append(o, "/proc/self/status: unreadable, errno %d (%s)\n", errno, strerror(errno));
        return;
    }
    char line[256];
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "Seccomp:", 8) == 0 || strncmp(line, "Seccomp_filters:", 16) == 0)
            append(o, "%s", line);
    }
    fclose(f);
}

static void append_file(struct out *o, const char *label, const char *path) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        append(o, "%s (%s): unreadable, errno %d (%s)\n", label, path, errno, strerror(errno));
        return;
    }
    char buf[256];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    int err = errno;
    close(fd);
    if (n < 0) {
        append(o, "%s (%s): read failed, errno %d (%s)\n", label, path, err, strerror(err));
        return;
    }
    buf[n] = '\0';
    // The SELinux context ends with a NUL; trim trailing NULs and newlines.
    while (n > 0 && (buf[n - 1] == '\0' || buf[n - 1] == '\n')) buf[--n] = '\0';
    append(o, "%s (%s): %s\n", label, path, buf);
}

JNIEXPORT jstring JNICALL
Java_com_pd2launcher_app_SeccompProbe_run(JNIEnv *env, jclass clazz) {
    (void)clazz;
    static struct out o;
    o.len = 0;
    o.buf[0] = '\0';

    append(&o, "uid %d pid %d\n", (int)getuid(), (int)getpid());
    append_status_lines(&o);
    append_file(&o, "SELinux enforce", "/sys/fs/selinux/enforce");
    append_file(&o, "SELinux context", "/proc/self/attr/current");
#ifdef __aarch64__
    for (size_t i = 0; i < sizeof(probes) / sizeof(probes[0]); i++) run_probe(&o, &probes[i]);
#else
    append(&o, "syscall probes: only built for aarch64\n");
#endif
    return (*env)->NewStringUTF(env, o.buf);
}
