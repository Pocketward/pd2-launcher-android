// The game's frame rate while the game is not on the screen: the app in the background, the screen
// off, or the app's own screens over the game. The Game view's copy paces the game while it runs
// (frame_copy.c; Mesa patch 0003 holds each present until the copy has taken the one before). With
// no copy nothing took the frames, Mesa did not wait, and the game drew as fast as it could for no
// one to see. This pacer takes the game's frames at a fixed rate instead, so Mesa lets one present
// through per interval and the game draws no more frames than that. It runs for the game
// environment's lifetime (BackgroundPacer.kt), and holds the game only while no copy runs. The
// copy and the pacer write the same words of the present counter's page (the last present taken
// at byte 64, pacing on at byte 128) only through this file, under one lock, so neither undoes
// the other: a copy that starts takes over at once, and a copy that stops hands the game back.
// Frames for the direct present skip these words (Mesa patch 0004 waits only for a free buffer):
// without a Game view the present server gives one buffer back per interval instead
// (pacing_background_interval). Logs under pd2-pacer: when the game leaves and comes back to the
// screen, and every 10 s in between, the presents a second it drew.
#include "background_pacer.h"

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

#define TAG "pd2-pacer"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
// The present counter's page (Mesa patches 0002 and 0003), as frame_copy.c maps it: the presents
// at byte 0 (Mesa's), the last one taken at byte 64, and pacing on at byte 128.
#define PACING_PAGE 4096
#define TAKEN_WORD (64 / 4)
#define PACED_WORD (128 / 4)
// Mesa waits at most 100 ms for a take, and after such a wait not again until one comes: the
// longest interval leaves room for a late wake-up, so a slow rate never turns into no pacing.
#define MAX_INTERVAL_NS 80000000LL
#define STATS_INTERVAL_NS 10000000000LL

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
// Under lock: the Game view copies that pace the game now (one, but a new surface's copy can start
// before the old one has stopped).
static int copies;
// Under lock: the pacer's own mapping of the page while it runs (NULL otherwise), its interval,
// and whether it is stopping.
static uint32_t *page;
static int64_t interval_ns;
static bool stopping;
static pthread_t thread;

static int64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

static uint32_t presents_now(const uint32_t *p) {
    return __atomic_load_n(&p[0], __ATOMIC_ACQUIRE);
}

static void wake_presenter(uint32_t *word) {
    syscall(__NR_futex, word, FUTEX_WAKE, INT_MAX, NULL, NULL, 0);
}

// Under lock: every present counted so far is taken, so Mesa sends the next one and then waits
// again; pacing is on.
static void take(uint32_t *p) {
    uint32_t presents = presents_now(p);
    if (__atomic_load_n(&p[TAKEN_WORD], __ATOMIC_RELAXED) == presents &&
        __atomic_load_n(&p[PACED_WORD], __ATOMIC_RELAXED) != 0) {
        return;
    }
    __atomic_store_n(&p[TAKEN_WORD], presents, __ATOMIC_RELEASE);
    __atomic_store_n(&p[PACED_WORD], 1u, __ATOMIC_RELEASE);
    wake_presenter(&p[TAKEN_WORD]);
}

// Under lock: Mesa waits for no one any more; a present that waits goes at once.
static void let_go(uint32_t *p) {
    __atomic_store_n(&p[PACED_WORD], 0u, __ATOMIC_RELEASE);
    wake_presenter(&p[TAKEN_WORD]);
}

void pacing_copy_started(uint32_t *copy_page) {
    pthread_mutex_lock(&lock);
    copies++;
    if (copy_page != NULL) take(copy_page);
    pthread_cond_signal(&changed);
    pthread_mutex_unlock(&lock);
}

void pacing_copy_stopped(uint32_t *copy_page) {
    pthread_mutex_lock(&lock);
    if (copies > 0) copies--;
    if (copies == 0) {
        // The pacer holds the game from a fresh take, so a present the copy left waiting goes now.
        if (page != NULL && !stopping) {
            take(page);
        } else if (copy_page != NULL) {
            let_go(copy_page);
        }
        pthread_cond_signal(&changed);
    }
    pthread_mutex_unlock(&lock);
}

int64_t pacing_background_interval(void) {
    pthread_mutex_lock(&lock);
    int64_t interval = page != NULL && !stopping ? interval_ns : 0;
    pthread_mutex_unlock(&lock);
    return interval;
}

static struct timespec at(int64_t ns) {
    struct timespec t = {(time_t)(ns / 1000000000LL), (long)(ns % 1000000000LL)};
    return t;
}

static void *run(void *data) {
    (void)data;
    pthread_mutex_lock(&lock);
    bool holding = false;
    int64_t next = 0, stats_ns = 0;
    uint32_t stats_presents = 0;
    while (!stopping) {
        if (copies > 0) {
            if (holding) LOG("the game is on the screen again: the Game view paces it");
            holding = false;
            pthread_cond_wait(&changed, &lock);
            continue;
        }
        int64_t now = now_ns();
        if (!holding) {
            LOG("the game is not on the screen: at most %.1f frames a second", 1e9 / (double)interval_ns);
            holding = true;
            next = now;
            stats_ns = now;
            stats_presents = presents_now(page);
        }
        if (now >= next) {
            take(page);
            next += interval_ns;
            // Woken late (the device asleep, say): a fresh start, not a burst of takes.
            if (next <= now) next = now + interval_ns;
        }
        if (now - stats_ns >= STATS_INTERVAL_NS) {
            uint32_t presents = presents_now(page);
            LOG("the game is not on the screen: %.1f presents/s", (double)(uint32_t)(presents - stats_presents) * 1e9 / (double)(now - stats_ns));
            stats_ns = now;
            stats_presents = presents;
        }
        struct timespec until = at(next);
        pthread_cond_clockwait(&changed, &lock, CLOCK_MONOTONIC, &until);
    }
    // No copy and no pacer: Mesa waits for no one.
    if (copies == 0) let_go(page);
    pthread_mutex_unlock(&lock);
    return NULL;
}

// Starts holding the game to one present per [interval] nanoseconds (at most MAX_INTERVAL_NS)
// whenever no Game view copy paces it, through the present counter's page in [counter]. False
// (logged) when it cannot, or when one runs already; an interval of 0 or less starts nothing.
JNIEXPORT jboolean JNICALL Java_com_pd2launcher_app_BackgroundPacer_nativeStart(JNIEnv *env, jclass cls, jstring counter, jlong interval) {
    (void)cls;
    if (interval <= 0 || counter == NULL) return JNI_FALSE;
    if (interval > MAX_INTERVAL_NS) interval = MAX_INTERVAL_NS;
    const char *file = (*env)->GetStringUTFChars(env, counter, NULL);
    if (file == NULL) return JNI_FALSE;
    uint32_t *map = NULL;
    int fd = open(file, O_RDWR | O_CLOEXEC);
    struct stat st;
    if (fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= PACING_PAGE) {
        void *m = mmap(NULL, PACING_PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (m != MAP_FAILED) map = m;
    }
    if (map == NULL) LOG("no pacing page at %s: %s", file, fd < 0 ? strerror(errno) : "too small or not mapped");
    if (fd >= 0) close(fd);
    (*env)->ReleaseStringUTFChars(env, counter, file);
    if (map == NULL) return JNI_FALSE;
    pthread_mutex_lock(&lock);
    bool running = page != NULL;
    int failed = 0;
    if (!running) {
        page = map;
        interval_ns = interval;
        stopping = false;
        failed = pthread_create(&thread, NULL, run, NULL);
        if (failed != 0) page = NULL;
        else pthread_setname_np(thread, "pd2-pacer");
    }
    pthread_mutex_unlock(&lock);
    if (running || failed != 0) {
        LOG("%s", running ? "a pacer runs already" : "cannot start the pacer thread");
        munmap(map, PACING_PAGE);
        return JNI_FALSE;
    }
    LOG("started: at most %.1f frames a second while the game is not on the screen", 1e9 / (double)interval);
    return JNI_TRUE;
}

// Stops the pacer: the game is no longer held while no copy runs.
JNIEXPORT void JNICALL Java_com_pd2launcher_app_BackgroundPacer_nativeStop(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    pthread_mutex_lock(&lock);
    bool running = page != NULL && !stopping;
    if (running) {
        stopping = true;
        pthread_cond_signal(&changed);
    }
    pthread_mutex_unlock(&lock);
    if (!running) return;
    pthread_join(thread, NULL);
    pthread_mutex_lock(&lock);
    munmap(page, PACING_PAGE);
    page = NULL;
    stopping = false;
    pthread_mutex_unlock(&lock);
    LOG("stopped");
}
