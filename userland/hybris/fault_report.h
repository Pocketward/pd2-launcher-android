/* Debug helpers of libvulkan_pd2hybris.so (included once by pd2hybris_icd.c).
 *
 * PD2_HYBRIS_SEGV=1: on SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGABRT print the fault address, the
 * registers, the /proc/self/maps lines of pc and lr, and a scan of the stack for code addresses
 * (bionic code has no debuggerd in a glibc process, and vendor code has no frame pointers), then
 * die with the default action. Not async-signal-safe; for test runs only.
 *
 * Vendor code installs signal handlers and blocks signals through bionic's libc (libhybris does
 * not translate those calls, its signal hooks are off); the ICD answers sigaction,
 * pthread_sigmask and sigprocmask itself (hook callback in pd2hybris_icd.c), translates bionic's
 * 8-byte sigset_t to glibc's, never lets vendor code install a handler for a fault signal (they
 * belong to Wine and Box64 in the game's process), and with PD2_HYBRIS_SEGV=1 keeps the fault
 * signals unblocked for this handler. */
#include <signal.h>
#include <ucontext.h>
#include <unistd.h>
#include <fcntl.h>

struct map_ent {
    unsigned long lo, hi, off;
    int exec;
    char path[160];
};
static struct map_ent g_maps[4096];
static int g_nmaps;

static void maps_load(void)
{
    int fd = open("/proc/self/maps", O_RDONLY);
    if (fd < 0)
        return;
    static char buf[1 << 21];
    ssize_t n, len = 0;
    while (len < (ssize_t)sizeof buf - 1 && (n = read(fd, buf + len, sizeof buf - 1 - len)) > 0)
        len += n;
    close(fd);
    buf[len] = 0;
    g_nmaps = 0;
    for (char *l = buf; l && *l && g_nmaps < (int)(sizeof g_maps / sizeof g_maps[0]);) {
        char *e = strchr(l, '\n');
        if (e)
            *e = 0;
        struct map_ent *m = &g_maps[g_nmaps];
        char perms[8] = "", path[512] = "";
        if (sscanf(l, "%lx-%lx %7s %lx %*s %*s %511[^\n]", &m->lo, &m->hi, perms, &m->off, path) >= 4) {
            m->exec = perms[2] == 'x';
            snprintf(m->path, sizeof m->path, "%s", path);
            g_nmaps++;
        }
        l = e ? e + 1 : NULL;
    }
}

static const struct map_ent *map_of(uintptr_t a)
{
    for (int i = 0; i < g_nmaps; i++)
        if (a >= g_maps[i].lo && a < g_maps[i].hi)
            return &g_maps[i];
    return NULL;
}

static void print_addr(const char *what, uintptr_t a)
{
    const struct map_ent *m = map_of(a);
    if (m)
        fprintf(stderr, "pd2hybris:   %s 0x%lx = %s + 0x%lx%s\n", what, (unsigned long)a,
                m->path[0] ? m->path : "[anon]", (unsigned long)(a - m->lo + m->off), m->exec ? "" : " (not code)");
    else
        fprintf(stderr, "pd2hybris:   %s 0x%lx (no mapping)\n", what, (unsigned long)a);
}

static void fault_handler(int sig, siginfo_t *si, void *uc_)
{
    maps_load();
    fprintf(stderr, "pd2hybris: FAULT signal %d code %d addr %p thread %ld\n", sig, si->si_code, si->si_addr,
            (long)gettid());
#if defined(__aarch64__)
    ucontext_t *uc = uc_;
    uintptr_t pc = uc->uc_mcontext.pc, lr = uc->uc_mcontext.regs[30], sp = uc->uc_mcontext.sp;
    print_addr("pc", pc);
    print_addr("lr", lr);
    for (int i = 0; i < 31; i += 4) {
        unsigned long long *r = uc->uc_mcontext.regs;
        fprintf(stderr, "pd2hybris:   x%-2d %016llx x%-2d %016llx x%-2d %016llx x%-2d %016llx\n", i, r[i], i + 1,
                i + 1 < 31 ? r[i + 1] : 0, i + 2, i + 2 < 31 ? r[i + 2] : 0, i + 3, i + 3 < 31 ? r[i + 3] : 0);
    }
    fprintf(stderr, "pd2hybris:   sp %016lx\n", (unsigned long)sp);
    const struct map_ent *sm = map_of(sp);
    int shown = 0;
    for (uintptr_t a = sp; sm && a + 8 <= sm->hi && a < sp + 16384 && shown < 40; a += 8) {
        uintptr_t v = *(uintptr_t *)a;
        const struct map_ent *m = map_of(v);
        if (m && m->exec) {
            char what[40];
            snprintf(what, sizeof what, "stack+0x%lx", (unsigned long)(a - sp));
            print_addr(what, v);
            shown++;
        }
    }
#else
    (void)uc_;
    (void)print_addr;
#endif
    fflush(stderr);
    signal(sig, SIG_DFL);
    raise(sig);
}

static int g_keep_faults;

static int fault_sig(int s)
{
    return s == SIGSEGV || s == SIGBUS || s == SIGILL || s == SIGFPE || s == SIGABRT;
}

/* ------------------------------------------------------------------ fault watch (always on)
 *
 * A crash in the phone's driver in the game's process: Wine turns a fault in native code into a
 * Windows exception, and the game ends as after a crash of its own, so the app could not tell the
 * two apart. The watch writes one line "<process> <pid> fault signal <n> at <file>+0x<offset>"
 * into the route's status file (PD2_HYBRIS_STATUS_FILE, the app's LibhybrisStatus) when the
 * faulting code (pc, or the caller in lr) is in a library that libhybris loaded (/system, /vendor,
 * /apex, /odm, /product, /system_ext: a glibc process has no other bionic code) or in libhybris or
 * this ICD, and then hands the signal on to the handler that was there before (Wine's), with the
 * same arguments: what happens next is as without the watch. Every other fault (Box64, Wine, the
 * game) only costs a lookup in a table of code ranges, which non-signal code refreshes
 * (fault_watch_refresh: after the HAL loads, after vkCreateDevice and the swapchain, and after the
 * first presents, as the driver loads more libraries). The handler itself only reads the table,
 * /proc/self/comm and the file, with async-signal-safe calls. */
#define FW_RANGES 1024
#define FW_SIGS 4
struct fw_range {
    uintptr_t lo, hi, off;
    char path[112];
};
static struct fw_range g_fw[2][FW_RANGES];
static int g_fw_n[2];
static int g_fw_cur = -1; /* the table the handler reads; atomic */
static pthread_mutex_t g_fw_lock = PTHREAD_MUTEX_INITIALIZER;
static const int k_fw_sigs[FW_SIGS] = { SIGSEGV, SIGBUS, SIGILL, SIGFPE };
static struct sigaction g_fw_old[FW_SIGS];
static char g_fw_status[512];
static volatile sig_atomic_t g_fw_written;

static int fw_counts(const char *path)
{
    static const char *const prefixes[] = { "/system/", "/vendor/", "/apex/", "/odm/", "/product/", "/system_ext/" };
    for (size_t i = 0; i < sizeof prefixes / sizeof prefixes[0]; i++)
        if (strncmp(path, prefixes[i], strlen(prefixes[i])) == 0)
            return 1;
    return strstr(path, "libvulkan_pd2hybris") != NULL || strstr(path, "libhybris") != NULL;
}

/* The executable mappings of the libraries that count, into the table the handler does not read. */
static void fault_watch_refresh(void)
{
    if (__atomic_load_n(&g_fw_cur, __ATOMIC_ACQUIRE) < 0 && !g_fw_status[0])
        return; /* not installed */
    pthread_mutex_lock(&g_fw_lock);
    int next = __atomic_load_n(&g_fw_cur, __ATOMIC_ACQUIRE) == 0 ? 1 : 0;
    FILE *f = fopen("/proc/self/maps", "re");
    int n = 0;
    if (f) {
        char line[768];
        while (n < FW_RANGES && fgets(line, sizeof line, f)) {
            unsigned long lo, hi, off;
            char perms[8], path[512] = "";
            if (sscanf(line, "%lx-%lx %7s %lx %*s %*s %511[^\n]", &lo, &hi, perms, &off, path) < 5)
                continue;
            if (perms[2] != 'x' || !fw_counts(path))
                continue;
            struct fw_range *r = &g_fw[next][n++];
            r->lo = lo;
            r->hi = hi;
            r->off = off;
            /* The file's name (the app's userland paths are longer than the field). */
            const char *base = strrchr(path, '/');
            snprintf(r->path, sizeof r->path, "%s", base ? base + 1 : path);
        }
        fclose(f);
    }
    g_fw_n[next] = n;
    __atomic_store_n(&g_fw_cur, next, __ATOMIC_RELEASE);
    pthread_mutex_unlock(&g_fw_lock);
}

static const struct fw_range *fw_find(int t, uintptr_t a)
{
    for (int i = 0; i < g_fw_n[t]; i++)
        if (a >= g_fw[t][i].lo && a < g_fw[t][i].hi)
            return &g_fw[t][i];
    return NULL;
}

static size_t fw_put(char *out, size_t at, size_t cap, const char *s)
{
    while (*s && at + 1 < cap)
        out[at++] = *s++;
    return at;
}

static size_t fw_put_num(char *out, size_t at, size_t cap, unsigned long v, int hex)
{
    char tmp[24];
    int k = 0;
    do {
        unsigned d = (unsigned)(v % (hex ? 16 : 10));
        tmp[k++] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
        v /= hex ? 16 : 10;
    } while (v && k < (int)sizeof tmp);
    while (k > 0 && at + 1 < cap)
        out[at++] = tmp[--k];
    return at;
}

/* One status line, in the signal handler: async-signal-safe calls only. */
static void fw_write(int sig, const struct fw_range *r, uintptr_t a, int caller)
{
    char line[400], comm[32] = "?";
    int cf = open("/proc/self/comm", O_RDONLY | O_CLOEXEC);
    if (cf >= 0) {
        ssize_t k = read(cf, comm, sizeof comm - 1);
        comm[k > 0 ? k : 1] = 0;
        for (char *c = comm; *c; c++)
            if (*c == '\n')
                *c = 0;
        close(cf);
    }
    const char *base = r->path;
    size_t at = 0;
    at = fw_put(line, at, sizeof line, comm);
    at = fw_put(line, at, sizeof line, " ");
    at = fw_put_num(line, at, sizeof line, (unsigned long)getpid(), 0);
    at = fw_put(line, at, sizeof line, " fault signal ");
    at = fw_put_num(line, at, sizeof line, (unsigned long)sig, 0);
    at = fw_put(line, at, sizeof line, caller ? " in code called from " : " at ");
    at = fw_put(line, at, sizeof line, base);
    at = fw_put(line, at, sizeof line, "+0x");
    at = fw_put_num(line, at, sizeof line, (unsigned long)(a - r->lo + r->off), 1);
    at = fw_put(line, at, sizeof line, "\n");
    int fd = open(g_fw_status, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd >= 0) {
        ssize_t w = write(fd, line, at);
        (void)w;
        close(fd);
    }
    ssize_t w = write(2, "pd2hybris: ", 11);
    w = write(2, line, at);
    (void)w;
}

static void fw_handler(int sig, siginfo_t *si, void *uc_)
{
    int saved = errno;
    int t = __atomic_load_n(&g_fw_cur, __ATOMIC_ACQUIRE);
    if (t >= 0 && !g_fw_written && g_fw_status[0]) {
        ucontext_t *uc = uc_;
#if defined(__aarch64__)
        uintptr_t pc = uc->uc_mcontext.pc, lr = uc->uc_mcontext.regs[30];
#elif defined(__x86_64__)
        uintptr_t pc = (uintptr_t)uc->uc_mcontext.gregs[REG_RIP], lr = 0; /* the desk test */
#endif
#if defined(__aarch64__) || defined(__x86_64__)
        const struct fw_range *r = fw_find(t, pc);
        int caller = 0;
        if (!r) {
            r = fw_find(t, lr);
            caller = 1;
        }
        if (r) {
            g_fw_written = 1;
            fw_write(sig, r, caller ? lr : pc, caller);
        }
#endif
    }
    errno = saved;
    int k = 0;
    while (k < FW_SIGS && k_fw_sigs[k] != sig)
        k++;
    if (k == FW_SIGS)
        return;
    const struct sigaction *o = &g_fw_old[k];
    if (o->sa_flags & SA_SIGINFO) {
        if (o->sa_sigaction) {
            o->sa_sigaction(sig, si, uc_);
            return;
        }
    } else if (o->sa_handler == SIG_IGN) {
        return;
    } else if (o->sa_handler != SIG_DFL && o->sa_handler) {
        o->sa_handler(sig);
        return;
    }
    /* No handler before: the default action, as without the watch (a fault comes again on return;
     * a raised signal is raised again). */
    signal(sig, SIG_DFL);
    if (si->si_code <= 0)
        raise(sig);
}

/* Installs the watch over the fault handlers that are there (Wine's in the game's process). */
static void fault_watch_install(void)
{
    const char *path = getenv("PD2_HYBRIS_STATUS_FILE");
    if (!path || !*path || strlen(path) >= sizeof g_fw_status)
        return;
    snprintf(g_fw_status, sizeof g_fw_status, "%s", path);
    fault_watch_refresh();
    for (int k = 0; k < FW_SIGS; k++) {
        struct sigaction sa;
        if (sigaction(k_fw_sigs[k], NULL, &g_fw_old[k]) != 0)
            continue;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = fw_handler;
        sa.sa_mask = g_fw_old[k].sa_mask;
        /* 0x04000000: SA_RESTORER, which glibc sets itself. */
        sa.sa_flags = (g_fw_old[k].sa_flags & ~(SA_RESETHAND | 0x04000000)) | SA_SIGINFO;
        sigaction(k_fw_sigs[k], &sa, NULL);
    }
}

static void fault_report_install(void)
{
    const char *e = getenv("PD2_HYBRIS_SEGV");
    if (!e || strcmp(e, "1") != 0)
        return;
    g_keep_faults = 1;
    static char altstack[64 * 1024];
    stack_t ss = { .ss_sp = altstack, .ss_size = sizeof altstack };
    sigaltstack(&ss, NULL);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = fault_handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_RESETHAND;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);
    sigaction(SIGFPE, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
}

/* bionic's LP64 signal types: sigset_t is one unsigned long; struct sigaction is
 * { handler; sigset_t sa_mask; int sa_flags; void (*sa_restorer)(void); }. */
struct bionic_sigaction {
    void *handler;
    unsigned long mask;
    int flags;
    void (*restorer)(void);
};

static void to_glibc_set(sigset_t *g, unsigned long b)
{
    sigemptyset(g);
    for (int s = 1; s <= 64; s++)
        if (b & (1UL << (s - 1)))
            sigaddset(g, s);
}

static unsigned long from_glibc_set(const sigset_t *g)
{
    unsigned long b = 0;
    for (int s = 1; s <= 64; s++)
        if (sigismember(g, s) == 1)
            b |= 1UL << (s - 1);
    return b;
}

/* Vendor code may not take over the fault signals: in the game's process they belong to Wine and
 * Box64 (exceptions, the dynarec). PD2_HYBRIS_VENDOR_SIGNALS=1 lets it (tests only). */
static int vendor_fault_handlers_allowed(void)
{
    static int v = -1;
    if (v < 0) {
        const char *e = getenv("PD2_HYBRIS_VENDOR_SIGNALS");
        v = e && strcmp(e, "1") == 0;
    }
    return v && !g_keep_faults;
}

__attribute__((unused)) static int pd2_bionic_sigaction(int sig, const struct bionic_sigaction *act, struct bionic_sigaction *old)
{
    int refuse = act && (fault_sig(sig) || sig == SIGTRAP) && !vendor_fault_handlers_allowed();
    LOGE("vendor sigaction(%d, handler %p, flags 0x%x)%s", sig, act ? act->handler : NULL, act ? act->flags : 0,
         refuse ? ": not installed" : "");
    if (refuse) {
        if (old)
            memset(old, 0, sizeof *old);
        return 0;
    }
    struct sigaction ga, go;
    memset(&ga, 0, sizeof ga);
    memset(&go, 0, sizeof go);
    if (act) {
        ga.sa_handler = (void (*)(int))act->handler;
        to_glibc_set(&ga.sa_mask, act->mask);
        ga.sa_flags = act->flags & ~0x04000000; /* SA_RESTORER: glibc sets its own */
    }
    int r = sigaction(sig, act ? &ga : NULL, old ? &go : NULL);
    if (old) {
        old->handler = (void *)go.sa_handler;
        old->mask = from_glibc_set(&go.sa_mask);
        old->flags = go.sa_flags;
        old->restorer = NULL;
    }
    return r;
}

static int mask_common(int how, const unsigned long *set, unsigned long *old, int is_pthread)
{
    sigset_t gs, go;
    if (set) {
        unsigned long b = *set;
        if (g_keep_faults && (how == SIG_BLOCK || how == SIG_SETMASK))
            b &= ~((1UL << (SIGSEGV - 1)) | (1UL << (SIGBUS - 1)) | (1UL << (SIGILL - 1)) | (1UL << (SIGFPE - 1)));
        to_glibc_set(&gs, b);
    }
    int r = is_pthread ? pthread_sigmask(how, set ? &gs : NULL, old ? &go : NULL)
                       : sigprocmask(how, set ? &gs : NULL, old ? &go : NULL);
    if (old)
        *old = from_glibc_set(&go);
    return r;
}

__attribute__((unused)) static int pd2_bionic_pthread_sigmask(int how, const unsigned long *set, unsigned long *old)
{
    return mask_common(how, set, old, 1);
}

__attribute__((unused)) static int pd2_bionic_sigprocmask(int how, const unsigned long *set, unsigned long *old)
{
    return mask_common(how, set, old, 0) == 0 ? 0 : -1;
}
