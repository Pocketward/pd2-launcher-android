// The app's clock: the virtual display's vertical blank (Mesa patch 0008). It ticks at the rate
// the player picked (Settings, Game, Frame rate: 60 or 30 frames a second; Full: the panel's
// highest rate) for the game environment's lifetime, on the screen and off it, whatever rate the
// phone's screen runs at. Each tick counts up the word at byte 256 of the present counter's page
// and wakes Mesa's waiters; the word at byte 320 is non-zero while it ticks. The game waits in Mesa
// for a tick before each present, in every present mode (vsync on, D2GL's default, or off), so it
// draws at most that many frames a second. While the
// game is not on the screen it uses its own background frame rate (x-input-relay's RELAY_FOCUS,
// GameView.kt). FrameClock.kt starts it; the Game view sets its rate. Logs under pd2-clock every
// 10 s: its ticks and the game's presents a second.
//
// It also keeps the word at byte 128 non-zero while a Game view copy runs (frame_copy.c), so Mesa
// writes the X screen's frames into its back pixmap and copies them into the window only after the
// copy's take (patches 0003 and 0007: no frame written over one the copy reads).
#include "frame_clock.h"
#include "frame_pacing.h"
#include "present_server.h"

#include <android/log.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdbool.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define TAG "pd2-clock"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define STATS_INTERVAL_NS 10000000000LL

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
// Under lock: the clock's own mapping of the page while it runs (NULL otherwise), its interval,
// the next tick, and whether it is stopping; the Game view copies that run (the word at byte 128).
static uint32_t *page;
static int64_t interval_ns;
static int64_t due_ns;
static bool stopping;
static pthread_t thread;
static int copies;

static int64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

static void wake(uint32_t *word) {
    syscall(__NR_futex, word, FUTEX_WAKE, INT_MAX, NULL, NULL, 0);
}

void pacing_copy_started(uint32_t *copy_page) {
    pthread_mutex_lock(&lock);
    copies++;
    if (copy_page != NULL) {
        __atomic_store_n(&copy_page[TAKEN_WORD], __atomic_load_n(&copy_page[0], __ATOMIC_ACQUIRE), __ATOMIC_RELEASE);
        __atomic_store_n(&copy_page[PACED_WORD], 1u, __ATOMIC_RELEASE);
        wake(&copy_page[TAKEN_WORD]);
    }
    pthread_mutex_unlock(&lock);
}

void pacing_copy_stopped(uint32_t *copy_page) {
    pthread_mutex_lock(&lock);
    if (copies > 0) copies--;
    // No copy reads the X screen any more: Mesa writes its frames into the window at once.
    if (copies == 0 && copy_page != NULL) {
        __atomic_store_n(&copy_page[PACED_WORD], 0u, __ATOMIC_RELEASE);
        wake(&copy_page[TAKEN_WORD]);
    }
    pthread_mutex_unlock(&lock);
}

static struct timespec at(int64_t ns) {
    struct timespec t = {(time_t)(ns / 1000000000LL), (long)(ns % 1000000000LL)};
    return t;
}

static void *run(void *data) {
    (void)data;
    pthread_mutex_lock(&lock);
    __atomic_store_n(&page[CLOCK_WORD], 1u, __ATOMIC_RELEASE);
    due_ns = now_ns();
    int64_t stats_ns = due_ns;
    uint32_t ticks = 0, stats_presents = __atomic_load_n(&page[0], __ATOMIC_ACQUIRE);
    while (!stopping) {
        int64_t now = now_ns();
        if (now >= due_ns) {
            __atomic_fetch_add(&page[TICK_WORD], 1u, __ATOMIC_RELEASE);
            wake(&page[TICK_WORD]);
            // The direct present's newest frame that waits for this tick goes to the screen.
            present_server_tick();
            ticks++;
            due_ns = clock_next(due_ns, now, interval_ns);
        }
        if (now - stats_ns >= STATS_INTERVAL_NS) {
            uint32_t presents = __atomic_load_n(&page[0], __ATOMIC_ACQUIRE);
            double seconds = (double)(now - stats_ns) / 1e9;
            LOG("%.1f ticks/s; the game %.1f presents/s", ticks / seconds, (uint32_t)(presents - stats_presents) / seconds);
            stats_ns = now;
            stats_presents = presents;
            ticks = 0;
        }
        struct timespec until = at(due_ns);
        pthread_cond_clockwait(&changed, &lock, CLOCK_MONOTONIC, &until);
    }
    // Mesa waits for no tick any more; a last tick wakes a waiter that is about to sleep too, so
    // it does not wait 100 ms and take the clock for stalled.
    __atomic_store_n(&page[CLOCK_WORD], 0u, __ATOMIC_RELEASE);
    __atomic_fetch_add(&page[TICK_WORD], 1u, __ATOMIC_RELEASE);
    wake(&page[TICK_WORD]);
    // A frame with vsync off that waits for a tick goes now: no clock holds the screen any more.
    present_server_tick();
    pthread_mutex_unlock(&lock);
    return NULL;
}

// Starts the clock at one tick per [interval] nanoseconds through the present counter's page in
// [counter]. False (logged) when it cannot, or when one runs already.
JNIEXPORT jboolean JNICALL Java_com_pd2launcher_app_FrameClock_nativeStart(JNIEnv *env, jclass cls, jstring counter, jlong interval) {
    (void)cls;
    if (interval <= 0 || counter == NULL) return JNI_FALSE;
    const char *file = (*env)->GetStringUTFChars(env, counter, NULL);
    if (file == NULL) return JNI_FALSE;
    uint32_t *map = NULL;
    int fd = open(file, O_RDWR | O_CLOEXEC);
    struct stat st;
    if (fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= PACING_PAGE) {
        void *m = mmap(NULL, PACING_PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (m != MAP_FAILED) map = m;
    }
    if (map == NULL) LOG("no present counter page at %s: %s", file, fd < 0 ? strerror(errno) : "too small or not mapped");
    if (fd >= 0) close(fd);
    (*env)->ReleaseStringUTFChars(env, counter, file);
    if (map == NULL) return JNI_FALSE;
    pthread_mutex_lock(&lock);
    // A clock that is stopping (nativeStop on another thread) is gone first.
    while (stopping) pthread_cond_wait(&changed, &lock);
    bool running = page != NULL;
    int failed = 0;
    if (!running) {
        page = map;
        interval_ns = interval;
        stopping = false;
        failed = pthread_create(&thread, NULL, run, NULL);
        if (failed != 0) page = NULL;
        else pthread_setname_np(thread, "pd2-clock");
    }
    pthread_mutex_unlock(&lock);
    if (running || failed != 0) {
        LOG("%s", running ? "a clock runs already" : "cannot start the clock thread");
        munmap(map, PACING_PAGE);
        return JNI_FALSE;
    }
    LOG("started: %.1f ticks a second", 1e9 / (double)interval);
    return JNI_TRUE;
}

// The clock ticks once per [interval] nanoseconds from now on (the next tick comes no later than
// one new interval from now).
JNIEXPORT void JNICALL Java_com_pd2launcher_app_FrameClock_nativeSetInterval(JNIEnv *env, jclass cls, jlong interval) {
    (void)env;
    (void)cls;
    if (interval <= 0) return;
    pthread_mutex_lock(&lock);
    if (page != NULL && interval != interval_ns) {
        interval_ns = interval;
        int64_t latest = now_ns() + interval;
        if (due_ns > latest) due_ns = latest;
        pthread_cond_broadcast(&changed);
        LOG("%.1f ticks a second", 1e9 / (double)interval);
    }
    pthread_mutex_unlock(&lock);
}

// Stops the clock: Mesa waits for no tick any more.
JNIEXPORT void JNICALL Java_com_pd2launcher_app_FrameClock_nativeStop(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    pthread_mutex_lock(&lock);
    if (stopping) {
        // Another thread stops it: it is gone when this returns, as after a stop of its own.
        while (stopping) pthread_cond_wait(&changed, &lock);
        pthread_mutex_unlock(&lock);
        return;
    }
    bool running = page != NULL;
    if (running) {
        stopping = true;
        pthread_cond_broadcast(&changed);
    }
    pthread_mutex_unlock(&lock);
    if (!running) return;
    pthread_join(thread, NULL);
    pthread_mutex_lock(&lock);
    munmap(page, PACING_PAGE);
    page = NULL;
    stopping = false;
    pthread_cond_broadcast(&changed);
    pthread_mutex_unlock(&lock);
    LOG("stopped");
}
