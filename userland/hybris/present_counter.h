/* The app's present counter page for libvulkan_pd2hybris.so's software swapchain (included once
 * by pd2hybris_icd.c). The same contract as our Mesa WSI patches (userland/patches/mesa
 * 0002, 0003, 0008), which the app's FrameCopy, FrameClock and PresentCounter use:
 *   PD2_PRESENT_COUNTER names a file the app makes, a page (4096 bytes) of 32-bit words:
 *   byte 0    the presents: one more for each frame the X server has (the app's fps counter, and
 *             its rule that a game on the phone's own driver that sent no frame failed);
 *   byte 64   the last present the Game view's copy took off the X screen (futex word), and
 *   byte 128  non-zero while that copy runs: then a present waits until the frame before was taken
 *             (at most SW_PACING_TIMEOUT_NS; a stalled copy is not waited for again until it takes);
 *   byte 192  non-zero while the app wants the frames in its own buffers (the direct present,
 *             PD2_PRESENT_SOCKET), zero while they go to the X screen;
 *   byte 256  the app's clock, its virtual vertical blank (futex word), and
 *   byte 320  non-zero while it ticks: a present of every mode (vsync on or off) waits for a tick
 *             since its last present, so the game shows at most one frame a tick (the player's
 *             frame rate), and while it ticks an acquire waits until the last presented image has
 *             passed that wait (pc_clock_holds).
 * Without the variable or the file nothing is counted or waited for. */
#include <linux/futex.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>

#define PC_PAGE 4096
#define PC_TAKEN_WORD (64 / 4)
#define PC_PACED_WORD (128 / 4)
#define PC_WANTED_WORD (192 / 4)
#define PC_TICK_WORD (256 / 4)
#define PC_CLOCK_WORD (320 / 4)
#define PC_TIMEOUT_NS (100LL * 1000 * 1000)

static uint32_t *g_pc;
static int g_pc_page;
static pthread_once_t g_pc_once = PTHREAD_ONCE_INIT;
/* A stalled copy: its taken count, with bit 32 set; 0 while the copy keeps up. */
static uint64_t g_pc_stall;

static void pc_open(void)
{
    const char *path = getenv("PD2_PRESENT_COUNTER");
    if (!path || !*path)
        return;
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0)
        return;
    struct stat st;
    if (fstat(fd, &st) == 0 && st.st_size >= 4) {
        size_t len = st.st_size >= PC_PAGE ? PC_PAGE : 4;
        void *map = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (map != MAP_FAILED) {
            g_pc = map;
            g_pc_page = len == PC_PAGE;
        }
    }
    close(fd);
}

/* Waits on the shared futex word until it is no longer [val] or [deadline] (CLOCK_MONOTONIC) has
 * passed: 0 on a wake or a changed word, -1 at the deadline. The page is shared between processes
 * (the app wakes it), so no FUTEX_PRIVATE_FLAG. */
static int pc_wait(uint32_t *word, uint32_t val, const struct timespec *deadline)
{
    struct timespec now, rel;
    clock_gettime(CLOCK_MONOTONIC, &now);
    long long ns = (deadline->tv_sec - now.tv_sec) * 1000000000LL + (deadline->tv_nsec - now.tv_nsec);
    if (ns <= 0)
        return -1;
    rel.tv_sec = ns / 1000000000LL;
    rel.tv_nsec = ns % 1000000000LL;
    long r = syscall(SYS_futex, word, FUTEX_WAIT, val, &rel, NULL, 0);
    if (r != 0 && errno == ETIMEDOUT)
        return -1;
    return 0;
}

static void pc_deadline(struct timespec *d)
{
    clock_gettime(CLOCK_MONOTONIC, d);
    d->tv_nsec += PC_TIMEOUT_NS % 1000000000LL;
    d->tv_sec += PC_TIMEOUT_NS / 1000000000LL + d->tv_nsec / 1000000000LL;
    d->tv_nsec %= 1000000000LL;
}

/* The app's clock as one swapchain last saw it. */
struct pc_clock {
    uint32_t tick;
    int stalled;
};

/* A present of every mode (FIFO: vsync on; IMMEDIATE and MAILBOX: vsync off) waits for a tick of
 * the app's clock since this swapchain's last present, as Mesa patch 0008 does. The game is held at
 * its acquire (pc_clock_holds). A clock that has not ticked for PC_TIMEOUT_NS is not waited for
 * again until it ticks, and a clock that stops (word 320 at 0, its waiters woken) is not waited
 * for. */
static void pc_present_clock(struct pc_clock *seen)
{
    pthread_once(&g_pc_once, pc_open);
    if (!g_pc_page)
        return;
    uint32_t *tick = &g_pc[PC_TICK_WORD];
    const uint32_t *running = &g_pc[PC_CLOCK_WORD];
    uint32_t now = __atomic_load_n(tick, __ATOMIC_ACQUIRE);
    if (now != seen->tick)
        seen->stalled = 0;
    if (!seen->stalled && now == seen->tick) {
        struct timespec deadline;
        pc_deadline(&deadline);
        while (now == seen->tick && __atomic_load_n(running, __ATOMIC_ACQUIRE)) {
            if (pc_wait(tick, now, &deadline) != 0) {
                seen->stalled = 1;
                break;
            }
            now = __atomic_load_n(tick, __ATOMIC_ACQUIRE);
        }
    }
    seen->tick = now;
}

/* At most one presented image waits for the app's clock, as Mesa patch 0008's sw_clock_acquire:
 * while the clock ticks, an acquire waits until each image that the game presented has passed
 * pc_present_clock ([waiting]: those that have not). Then the game draws its next frame after the
 * tick that shows its last frame, and the frame on the screen is at most about one tick old.
 * Without this wait a game faster than the clock fills the present thread's queue, and a frame
 * shows one tick later for each image before it. An acquire that has no time to wait ([timeout] 0)
 * is not held. */
static int pc_clock_holds(uint32_t waiting, uint64_t timeout)
{
    pthread_once(&g_pc_once, pc_open);
    return waiting && timeout && g_pc_page && __atomic_load_n(&g_pc[PC_CLOCK_WORD], __ATOMIC_ACQUIRE);
}

/* Any present waits until the Game view's copy has taken the frame before, while that copy runs. */
static void pc_present_paced(void)
{
    pthread_once(&g_pc_once, pc_open);
    if (!g_pc_page)
        return;
    uint32_t *taken = &g_pc[PC_TAKEN_WORD];
    const uint32_t *paced = &g_pc[PC_PACED_WORD];
    struct timespec deadline;
    pc_deadline(&deadline);
    while (__atomic_load_n(paced, __ATOMIC_ACQUIRE)) {
        uint32_t seen = __atomic_load_n(taken, __ATOMIC_ACQUIRE);
        uint32_t presents = __atomic_load_n(g_pc, __ATOMIC_RELAXED);
        uint64_t stall = __atomic_load_n(&g_pc_stall, __ATOMIC_RELAXED);
        if ((int32_t)(presents - seen) <= 0) {
            if (stall)
                __atomic_store_n(&g_pc_stall, 0, __ATOMIC_RELAXED);
            return;
        }
        if (stall == ((1ULL << 32) | seen))
            return;
        if (stall)
            __atomic_store_n(&g_pc_stall, 0, __ATOMIC_RELAXED);
        if (pc_wait(taken, seen, &deadline) != 0) {
            __atomic_store_n(&g_pc_stall, (1ULL << 32) | __atomic_load_n(taken, __ATOMIC_ACQUIRE), __ATOMIC_RELAXED);
            return;
        }
    }
}

/* Whether the app wants the game's frames in its own buffers now (the direct present), else on the
 * X screen: the word at byte 192, which the app sets while the game's window is alone on the X
 * screen (Mesa patch 0004). Without the page, always. */
static int pc_direct_wanted(void)
{
    pthread_once(&g_pc_once, pc_open);
    if (!g_pc_page)
        return 1;
    return __atomic_load_n(&g_pc[PC_WANTED_WORD], __ATOMIC_ACQUIRE) != 0;
}

/* One frame more: the X server or the app's buffer has it. */
static void pc_present_counted(void)
{
    pthread_once(&g_pc_once, pc_open);
    if (g_pc)
        __atomic_fetch_add(g_pc, 1, __ATOMIC_RELEASE);
}
