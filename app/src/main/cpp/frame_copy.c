// The copy path (the base of the Game view): Xvfb's framebuffer (a memfd, or its -fbdir file
// when no memfd could be made: 32-bit BGRX after the XWD header) copied into a SurfaceView's
// window through ANativeWindow, on a display frame (AChoreographer on the copy thread's own
// looper) whose framebuffer changed: a hash of it spares the copy and the compositor a repeat.
// The game's own frames are counted where it sends them, by the App userland's Mesa (patch
// 0002: a 32-bit counter in a shared file); the copy reads that count for the fps counter, and
// hashes only on a display frame that a present came before (or while none come: a picture drawn
// outside Mesa's presentation). It does not copy on each present: a display frame shows one
// present at most whichever way they are taken, and the game often presents the same picture
// again, which the hash skips.
// The copy also paces the game (Mesa patch 0003): in the same file it says the last present it
// has taken off the X screen, and Mesa holds each present (vsync on or off) back until the one
// before is taken, so the game draws one frame per display frame and the X server never writes a
// frame over one the copy is reading. At a reduced frame rate (Settings, Game) only some display
// frames take (frame_pacing.h): every so many, evenly, where the interval is a whole number of the
// display's frames, else the first one at or after each due time, so the game draws that many
// frames a second on average. While no copy runs, background_pacer.c paces the game.
// While the game's own buffers are on the screen (present_server.cpp, Mesa patch 0004) the copy
// rests: its layer is under theirs and the X screen does not get the game's frames. A windowed
// game's buffers cover only its window, so the copy then still looks at the X screen every 100 ms
// for what shows around it.
#include <android/choreographer.h>
#include <android/log.h>
#include <android/looper.h>
#include <android/native_window.h>
#include <android/native_window_jni.h>
#if defined(__aarch64__)
#include <arm_neon.h>
#endif
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include "background_pacer.h"
#include "frame_layer.h"
#include "frame_pacing.h"
#include "present_server.h"

#define TAG "pd2-gameview"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define STATS_INTERVAL_NS 5000000000LL
// The largest X screen side taken (FrameCopy.kt's XwdHeader.MAX_SIDE).
#define MAX_SIDE 16384
// The present counter's page (Mesa patches 0002 and 0003): the presents at byte 0 (Mesa's), the
// last one the copy has taken at byte 64 and, at byte 128, non-zero while the copy runs.
#define PACING_PAGE 4096
#define TAKEN_WORD (64 / 4)
#define PACED_WORD (128 / 4)
// No present for this long: the picture is drawn outside Mesa's presentation (a Wine or GDI window,
// the official launcher), so every display frame is hashed again.
#define PRESENTS_QUIET_NS 250000000LL
// While presents come, a display frame without one is still hashed this often, for what draws
// over the game outside them (a Wine window, the X cursor).
#define SLOW_CHECK_NS 100000000LL

struct copy {
    ANativeWindow *window;
    uint8_t *map;
    size_t map_len;
    const uint32_t *pixels;
    int width, height, stride; // stride in pixels
    int transform; // the buffers' ANATIVEWINDOW_TRANSFORM_*: 0, or the rotation they are stored in
    struct frame_layer *layer; // the child layer the frames go to; NULL: they go through the window
    int pending; // a changed frame waits for a free layer buffer
    pthread_t thread;
    pthread_mutex_t lock;
    pthread_cond_t ready;
    ALooper *looper;
    int started;
    atomic_bool stop;
    // Totals since the start, and the values at the last stats line.
    int64_t start_ns, last_ns;
    uint64_t presented, copy_ns, copy_max_ns;
    _Alignas(8) uint64_t changed; // atomic: nativeChangedFrames reads it on the main thread
    uint64_t last_presented, last_changed, last_copy_ns;
    uint64_t previous_hash;
    uint64_t checks, check_ns, last_checks, last_check_ns;
    uint64_t hashes, last_hashes;
    int64_t last_hash_ns, last_present_ns;
    // The game's presents (Mesa patch 0002), NULL without the counter file; [pacing] the counter's
    // page (patch 0003), NULL when the file is shorter.
    uint32_t *presents;
    uint32_t *pacing;
    size_t presents_len;
    uint32_t seen_presents, last_presents;
    // Checks that found a present since the one before (the presents shown: one a display frame),
    // presents a check never saw (more than one between two checks: the later overwrote the one
    // before in the framebuffer), and checks that found a present but the same picture.
    _Alignas(8) uint64_t fresh; // atomic: nativeShownPresents reads it on the main thread
    uint64_t unseen, repeats, last_fresh, last_unseen, last_repeats;
    // A reduced frame rate: the display frames that take (frame_pacing.h); for the stats, those
    // that did not (they wait for the next due time), the last one that took and the longest gap
    // between two that took.
    struct take_schedule takes;
    uint64_t waits, last_waits;
    int64_t last_taken_ns, take_gap_max_ns;
};

static int64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

#define TILE 32

static inline uint32_t swizzle(uint32_t p) {
    return 0xFF000000u | (p & 0x0000FF00u) | ((p >> 16) & 0xFFu) | ((p & 0xFFu) << 16);
}

// One row: BGRX (0xXXRRGGBB as a little-endian word) to RGBX/RGBA (0xFFBBGGRR).
static void swizzle_row(uint32_t *restrict dst, const uint32_t *restrict src, int n) {
    for (int x = 0; x < n; x++) dst[x] = swizzle(src[x]);
}

// One pixel of a turned copy (see copy_rotated).
static inline void put_turned(const struct copy *c, uint32_t *dst, int dst_stride, int w, int h, int x, int y) {
    uint32_t p = swizzle(c->pixels[(size_t)y * c->stride + x]);
    if (c->transform == ANATIVEWINDOW_TRANSFORM_ROTATE_90) dst[(size_t)(w - 1 - x) * dst_stride + y] = p;
    else dst[(size_t)x * dst_stride + (h - 1 - y)] = p;
}

#if defined(__aarch64__)
// BGRX to RGBX/RGBA for four pixels: bytes B G R X to R G B, then alpha 0xFF.
static inline uint32x4_t swizzle4(uint32x4_t p) {
    static const uint8_t order[16] = {2, 1, 0, 255, 6, 5, 4, 255, 10, 9, 8, 255, 14, 13, 12, 255};
    uint8x16_t shuffled = vqtbl1q_u8(vreinterpretq_u8_u32(p), vld1q_u8(order));
    return vorrq_u32(vreinterpretq_u32_u8(shuffled), vdupq_n_u32(0xFF000000u));
}

// Rows a, b, c, d of a 4x4 block become its columns.
static inline void transpose4(uint32x4_t *a, uint32x4_t *b, uint32x4_t *c, uint32x4_t *d) {
    uint32x4x2_t ab = vtrnq_u32(*a, *b);
    uint32x4x2_t cd = vtrnq_u32(*c, *d);
    *a = vcombine_u32(vget_low_u32(ab.val[0]), vget_low_u32(cd.val[0]));
    *b = vcombine_u32(vget_low_u32(ab.val[1]), vget_low_u32(cd.val[1]));
    *c = vcombine_u32(vget_high_u32(ab.val[0]), vget_high_u32(cd.val[0]));
    *d = vcombine_u32(vget_high_u32(ab.val[1]), vget_high_u32(cd.val[1]));
}
#endif

// The frame stored turned in a buffer of height x width (dst_w x dst_h as the buffer has it), for
// a buffer transform of ROTATE_90 (the compositor turns it a quarter clockwise: stored a quarter
// counter-clockwise, (x, y) at row w-1-x, column y) or ROTATE_270 (stored a quarter clockwise,
// (x, y) at row x, column h-1-y): then the panel takes it without a rotation of its own. In
// 32-pixel tiles so the source's columns are read from the cache, 4x4 blocks with NEON.
static void copy_rotated(const struct copy *c, uint32_t *dst, int dst_stride, int dst_w, int dst_h) {
    int w = c->width < dst_h ? c->width : dst_h;
    int h = c->height < dst_w ? c->height : dst_w;
    int quarter = c->transform == ANATIVEWINDOW_TRANSFORM_ROTATE_90;
    (void)quarter; // only the NEON blocks ask
    for (int x0 = 0; x0 < w; x0 += TILE) {
        int x1 = x0 + TILE < w ? x0 + TILE : w;
        for (int y0 = 0; y0 < h; y0 += TILE) {
            int y1 = y0 + TILE < h ? y0 + TILE : h;
            for (int y = y0; y < y1; y += 4) {
                for (int x = x0; x < x1; x += 4) {
#if defined(__aarch64__)
                    if (x + 4 <= x1 && y + 4 <= y1) {
                        const uint32_t *src = c->pixels + (size_t)y * c->stride + x;
                        uint32x4_t r0 = swizzle4(vld1q_u32(src));
                        uint32x4_t r1 = swizzle4(vld1q_u32(src + c->stride));
                        uint32x4_t r2 = swizzle4(vld1q_u32(src + 2 * (size_t)c->stride));
                        uint32x4_t r3 = swizzle4(vld1q_u32(src + 3 * (size_t)c->stride));
                        if (quarter) {
                            // Columns x..x+3 (y upwards) to rows w-1-x down to w-4-x, at column y.
                            transpose4(&r0, &r1, &r2, &r3);
                            uint32_t *out = dst + (size_t)(w - 1 - x) * dst_stride + y;
                            vst1q_u32(out, r0);
                            vst1q_u32(out - dst_stride, r1);
                            vst1q_u32(out - 2 * (size_t)dst_stride, r2);
                            vst1q_u32(out - 3 * (size_t)dst_stride, r3);
                        } else {
                            // Columns x..x+3 (y downwards) to rows x..x+3, at column h-4-y.
                            transpose4(&r3, &r2, &r1, &r0);
                            uint32_t *out = dst + (size_t)x * dst_stride + (h - 4 - y);
                            vst1q_u32(out, r3);
                            vst1q_u32(out + dst_stride, r2);
                            vst1q_u32(out + 2 * (size_t)dst_stride, r1);
                            vst1q_u32(out + 3 * (size_t)dst_stride, r0);
                        }
                        continue;
                    }
#endif
                    for (int yy = y; yy < y + 4 && yy < y1; yy++) {
                        for (int xx = x; xx < x + 4 && xx < x1; xx++) put_turned(c, dst, dst_stride, w, h, xx, yy);
                    }
                }
            }
        }
    }
}

// A position-sensitive (Fletcher-style, four lanes) hash of the frame, so a frame whose content
// changed can be told from a repeat by reading it only, a fraction of a copy's cost.
static uint64_t hash_frame(const struct copy *c) {
    uint32_t a[4] = {0}, b[4] = {0};
#if defined(__aarch64__)
    uint32x4_t va = vdupq_n_u32(0), vb = vdupq_n_u32(0);
#endif
    for (int y = 0; y < c->height; y++) {
        const uint32_t *row = c->pixels + (size_t)y * c->stride;
        int x = 0;
#if defined(__aarch64__)
        for (; x + 16 <= c->width; x += 16) {
            uint32x4x4_t p = vld1q_u32_x4(row + x);
            va = vaddq_u32(va, p.val[0]);
            vb = vaddq_u32(vb, va);
            va = vaddq_u32(va, p.val[1]);
            vb = vaddq_u32(vb, va);
            va = vaddq_u32(va, p.val[2]);
            vb = vaddq_u32(vb, va);
            va = vaddq_u32(va, p.val[3]);
            vb = vaddq_u32(vb, va);
        }
#endif
        for (; x < c->width; x++) {
            a[x & 3] += row[x];
            b[x & 3] += a[x & 3];
        }
    }
#if defined(__aarch64__)
    uint32_t na[4], nb[4];
    vst1q_u32(na, va);
    vst1q_u32(nb, vb);
    for (int i = 0; i < 4; i++) {
        a[i] += na[i];
        b[i] += nb[i];
    }
#endif
    uint64_t h = 0;
    for (int i = 0; i < 4; i++) h = (h * 1000003u) ^ (((uint64_t)b[i] << 32) | a[i]);
    return h;
}

static uint32_t presents_now(const struct copy *c) {
    return c->presents != NULL ? __atomic_load_n(c->presents, __ATOMIC_ACQUIRE) : 0;
}

static void wake_presenter(uint32_t *word) {
    syscall(__NR_futex, word, FUTEX_WAKE, INT_MAX, NULL, NULL, 0);
}

// The copy has what it needs of the game's present [presents] (copied it, or found the same
// picture): Mesa may send the next one.
static void taken(struct copy *c, uint32_t presents) {
    if (c->pacing == NULL || __atomic_load_n(&c->pacing[TAKEN_WORD], __ATOMIC_RELAXED) == presents) return;
    __atomic_store_n(&c->pacing[TAKEN_WORD], presents, __ATOMIC_RELEASE);
    wake_presenter(&c->pacing[TAKEN_WORD]);
}

// Mesa waits for the copy only while it runs; on [on] off, the background pacer holds the game
// (background_pacer.c), or a waiting present goes at once. Counted with or without the page, so
// the pacer never paces beside a running copy.
static void set_paced(struct copy *c, int on) {
    if (on) pacing_copy_started(c->pacing);
    else pacing_copy_stopped(c->pacing);
}

static void stats(struct copy *c, int64_t now) {
    double window = (double)(now - c->last_ns) / 1e9;
    uint64_t presented = c->presented - c->last_presented;
    double copy_ms = presented ? (double)(c->copy_ns - c->last_copy_ns) / presented / 1e6 : 0;
    uint64_t checks = c->checks - c->last_checks;
    // Display frames that waited for the reduced frame rate's next due time (frame_pacing.h).
    uint64_t waits = c->waits - c->last_waits;
    // Per hash: a display frame that skips the hash costs next to nothing.
    uint64_t hashes = c->hashes - c->last_hashes;
    double check_ms = hashes ? (double)(c->check_ns - c->last_check_ns) / hashes / 1e6 : 0;
    uint32_t presents = presents_now(c);
    if (c->presents != NULL) {
        LOG("%dx%d: game %.1f presents/s; copy %.1f display frames/s, %.1f taken/s (longest gap %.1f ms), "
            "%.1f new/s, %.1f presented/s, presents shown %.1f/s, unseen %.1f/s, repeats %.1f/s, hashed %.1f/s, %s",
            c->width, c->height, (uint32_t)(presents - c->last_presents) / window, (checks + waits) / window,
            checks / window, (double)c->take_gap_max_ns / 1e6, (c->changed - c->last_changed) / window,
            presented / window, (c->fresh - c->last_fresh) / window, (c->unseen - c->last_unseen) / window,
            (c->repeats - c->last_repeats) / window, hashes / window, c->pacing != NULL ? "pacing" : "not pacing");
    }
    c->take_gap_max_ns = 0;
    LOG("%dx%d: %.1f presented/s, %.1f new/s, copy %.2f ms avg; totals: %llu presented, %llu new, "
        "copy %.2f ms avg, %.2f ms max, %.1f s; check %.2f ms avg",
        c->width, c->height, presented / window, (c->changed - c->last_changed) / window, copy_ms,
        (unsigned long long)c->presented, (unsigned long long)c->changed,
        c->presented ? (double)c->copy_ns / c->presented / 1e6 : 0, (double)c->copy_max_ns / 1e6,
        (double)(now - c->start_ns) / 1e9, check_ms);
    c->last_presents = presents;
    c->last_fresh = c->fresh;
    c->last_unseen = c->unseen;
    c->last_repeats = c->repeats;
    c->last_ns = now;
    c->last_presented = c->presented;
    c->last_changed = c->changed;
    c->last_copy_ns = c->copy_ns;
    c->last_checks = c->checks;
    c->last_waits = c->waits;
    c->last_check_ns = c->check_ns;
    c->last_hashes = c->hashes;
}

static void counted(struct copy *c, int64_t begin) {
    int64_t took = now_ns() - begin;
    c->presented++;
    c->copy_ns += took;
    if ((uint64_t)took > c->copy_max_ns) c->copy_max_ns = took;
}

// The frame into [dst] (a buffer of dst_w x dst_h pixels), as the buffers store it.
static void write_frame(const struct copy *c, uint32_t *dst, int dst_stride, int dst_w, int dst_h) {
    if (c->transform != 0) {
        copy_rotated(c, dst, dst_stride, dst_w, dst_h);
        return;
    }
    int w = dst_w < c->width ? dst_w : c->width;
    int h = dst_h < c->height ? dst_h : c->height;
    for (int y = 0; y < h; y++) swizzle_row(dst + (size_t)y * dst_stride, c->pixels + (size_t)y * c->stride, w);
}

// The X screen into the layer (or the window) when it [changed], or a frame that waited for a free
// layer buffer; [begin] is when the copy began. 0 when the window's format cannot take it (the
// copy stops).
static int put(struct copy *c, int changed, int64_t begin) {
    ANativeWindow_Buffer buffer;
    uint32_t *pixels;
    int stride;
    if (c->layer != NULL) {
        // A frame no buffer was free for is copied on the next display frame.
        if ((changed || c->pending) && frame_layer_lock(c->layer, &pixels, &stride)) {
            write_frame(c, pixels, stride, c->transform ? c->height : c->width, c->transform ? c->width : c->height);
            frame_layer_post(c->layer);
            c->pending = 0;
            counted(c, begin);
        } else if (changed) {
            c->pending = 1;
        }
    } else if (changed && ANativeWindow_lock(c->window, &buffer, NULL) == 0) {
        if (buffer.format != WINDOW_FORMAT_RGBX_8888 && buffer.format != WINDOW_FORMAT_RGBA_8888) {
            LOG("window format %d is not RGBX/RGBA; not copying", buffer.format);
            ANativeWindow_unlockAndPost(c->window);
            return 0;
        }
        write_frame(c, (uint32_t *)buffer.bits, buffer.stride, buffer.width, buffer.height);
        ANativeWindow_unlockAndPost(c->window);
        counted(c, begin);
    }
    return 1;
}

static void on_frame(int64_t frame_time_nanos, void *data) {
    struct copy *c = data;
    if (atomic_load(&c->stop)) return;
    int64_t checked = now_ns();
    uint32_t presents = presents_now(c);
    uint32_t since = presents - c->seen_presents;
    if (since > 0) c->last_present_ns = checked;
    // Every display frame: the copy's display period, and the direct present's display frame (at a
    // reduced frame rate it shows a waiting buffer on the display frames that take one).
    take_display(&c->takes, frame_time_nanos);
    present_server_vsync(frame_time_nanos);
    int direct = present_server_showing();
    if (direct) {
        // Every present counted as shown (the compositor shows each of the game's buffers, back
        // pressure drops none).
        if (c->checks > 0 && since > 0) __atomic_fetch_add(&c->fresh, since, __ATOMIC_RELAXED);
        c->seen_presents = presents;
        c->checks++;
        // The copy path's gaps start again when the game's frames come back to the X screen.
        c->last_taken_ns = 0;
        taken(c, presents);
        // Over the whole view nothing is read or copied. Over a windowed game's window only, the
        // X screen around it is checked as for a window drawn over the game, every 100 ms (the
        // game's own window there keeps its last frame on the X screen, hidden by the layer).
        if (direct == 2 && (c->pending || checked - c->last_hash_ns >= SLOW_CHECK_NS)) {
            uint64_t h = hash_frame(c);
            int changed = h != c->previous_hash;
            c->previous_hash = h;
            c->last_hash_ns = checked;
            c->hashes++;
            int64_t begin = now_ns();
            c->check_ns += begin - checked;
            if (changed) __atomic_fetch_add(&c->changed, 1, __ATOMIC_RELAXED);
            if (!put(c, changed, begin)) return;
        }
        if (checked - c->last_ns >= STATS_INTERVAL_NS) stats(c, checked);
        AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, c);
        return;
    }
    // A reduced frame rate: a display frame before the next take is due neither reads nor takes,
    // so Mesa holds the game's next present until the display frame that does. A changed picture
    // that waits for a free buffer is copied (and taken) on the next display frame anyway.
    if (!take_due(&c->takes, frame_time_nanos, c->pending)) {
        c->waits++;
        if (checked - c->last_ns >= STATS_INTERVAL_NS) stats(c, checked);
        AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, c);
        return;
    }
    if (c->last_taken_ns != 0 && frame_time_nanos - c->last_taken_ns > c->take_gap_max_ns) {
        c->take_gap_max_ns = frame_time_nanos - c->last_taken_ns;
    }
    c->last_taken_ns = frame_time_nanos;
    // Without a present since the last display frame the picture changed only if something drew
    // outside Mesa's presentation: while presents come that shows with the next one or the slow
    // check; once they stop, every display frame is hashed again.
    int hash = c->presents == NULL || c->checks == 0 || since > 0 || c->pending ||
               checked - c->last_present_ns > PRESENTS_QUIET_NS || checked - c->last_hash_ns >= SLOW_CHECK_NS;
    int changed = 0;
    if (hash) {
        uint64_t h = hash_frame(c);
        changed = c->checks == 0 || h != c->previous_hash;
        c->previous_hash = h;
        c->last_hash_ns = checked;
        c->hashes++;
    }
    if (c->checks > 0) {
        if (since > 0) __atomic_fetch_add(&c->fresh, 1, __ATOMIC_RELAXED);
        if (since > 1) c->unseen += since - 1;
        if (since > 0 && !changed) c->repeats++;
    }
    c->seen_presents = presents;
    c->checks++;
    int64_t begin = now_ns();
    c->check_ns += begin - checked;
    // Read by nativeChangedFrames on the main thread.
    if (changed) __atomic_fetch_add(&c->changed, 1, __ATOMIC_RELAXED);
    if (!put(c, changed, begin)) return;
    // A changed picture no buffer was free for holds the game until it is copied.
    if (!c->pending) taken(c, presents);
    int64_t now = now_ns();
    if (now - c->last_ns >= STATS_INTERVAL_NS) stats(c, now);
    AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, c);
}

static void *run(void *data) {
    struct copy *c = data;
    ALooper *looper = ALooper_prepare(0);
    ALooper_acquire(looper);
    pthread_mutex_lock(&c->lock);
    c->looper = looper;
    c->started = 1;
    pthread_cond_signal(&c->ready);
    pthread_mutex_unlock(&c->lock);
    c->start_ns = c->last_ns = now_ns();
    c->last_presents = c->seen_presents = presents_now(c);
    set_paced(c, 1);
    AChoreographer_postFrameCallback64(AChoreographer_getInstance(), on_frame, c);
    while (!atomic_load(&c->stop)) ALooper_pollOnce(-1, NULL, NULL, NULL);
    stats(c, now_ns());
    return NULL;
}

static void release(struct copy *c) {
    frame_layer_destroy(c->layer);
    if (c->presents != NULL) munmap(c->presents, c->presents_len);
    if (c->map != NULL && c->map != MAP_FAILED) munmap(c->map, c->map_len);
    if (c->window != NULL) ANativeWindow_release(c->window);
    pthread_mutex_destroy(&c->lock);
    pthread_cond_destroy(&c->ready);
    free(c);
}

// The present counter in [path] (Mesa patch 0002) into [c], with its pacing words when the file is
// a page long (patch 0003); nothing when it cannot be mapped.
static void map_presents(JNIEnv *env, jstring path, struct copy *c) {
    if (path == NULL) return;
    const char *file = (*env)->GetStringUTFChars(env, path, NULL);
    if (file == NULL) return;
    int fd = open(file, O_RDWR | O_CLOEXEC);
    struct stat st;
    if (fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= (off_t)sizeof(uint32_t)) {
        size_t len = st.st_size >= PACING_PAGE ? PACING_PAGE : sizeof(uint32_t);
        void *map = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (map != MAP_FAILED) {
            c->presents = map;
            c->presents_len = len;
            if (len == PACING_PAGE) c->pacing = map;
        }
    }
    if (c->presents == NULL) LOG("no present counter at %s: %s", file, fd < 0 ? strerror(errno) : "too small");
    if (fd >= 0) close(fd);
    (*env)->ReleaseStringUTFChars(env, path, file);
}

// Maps the framebuffer [fd] (taken: closed here) and starts copying its pixels ([offset] bytes in,
// [width]x[height], [stride] bytes per line) into [surface]'s window, stored turned by [transform]
// (ANATIVEWINDOW_TRANSFORM_ROTATE_90 or _270, else 0: as is); [presents] names the game's present
// counter file, or is null; [frame_rate] is the rate the layer asks the display for (0: none);
// [take_interval] the shortest time between two frames taken, in nanoseconds, at a reduced frame
// rate (0: one a display frame). Returns a handle for nativeStop, or 0 (logged) on failure.
JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_FrameCopy_nativeStart(
    JNIEnv *env, jclass cls, jobject surface, jint fd, jlong offset, jint width, jint height, jint stride,
    jint transform, jstring presents, jfloat frame_rate, jlong take_interval) {
    (void)cls;
    struct copy *c = calloc(1, sizeof(*c));
    if (c == NULL) {
        if (fd >= 0) close(fd);
        return 0;
    }
    pthread_mutex_init(&c->lock, NULL);
    pthread_cond_init(&c->ready, NULL);
    c->width = width;
    c->height = height;
    c->stride = stride / 4;
    c->takes.interval_ns = take_interval > 0 ? take_interval : 0;

    // The header is Xvfb's, in memory a guest can write: every size is checked, and the stride
    // against the width in 64 bits (width * 4 in an int overflows).
    if (width <= 0 || height <= 0 || width > MAX_SIDE || height > MAX_SIDE || stride <= 0 || stride % 4 != 0 ||
        (int64_t)stride < (int64_t)width * 4 || offset < 0) {
        LOG("framebuffer header refused: %dx%d, %d bytes a line at offset %lld", width, height, stride, (long long)offset);
        if (fd >= 0) close(fd);
        release(c);
        return 0;
    }

    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0) {
        LOG("no framebuffer: %s", fd < 0 ? "no descriptor" : strerror(errno));
        if (fd >= 0) close(fd);
        release(c);
        return 0;
    }
    if ((uint64_t)offset + (uint64_t)stride * (uint64_t)height > (uint64_t)st.st_size) {
        LOG("framebuffer of %lld bytes is smaller than %dx%d at offset %lld", (long long)st.st_size, width, height, (long long)offset);
        close(fd);
        release(c);
        return 0;
    }
    c->map_len = (size_t)st.st_size;
    c->map = mmap(NULL, c->map_len, PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    if (c->map == MAP_FAILED) {
        LOG("mmap failed: %s", strerror(errno));
        release(c);
        return 0;
    }
    c->pixels = (const uint32_t *)(c->map + offset);
    map_presents(env, presents, c);

    c->window = ANativeWindow_fromSurface(env, surface);
    if (c->window == NULL) {
        LOG("no window for the surface");
        release(c);
        return 0;
    }
    int turned = transform == ANATIVEWINDOW_TRANSFORM_ROTATE_90 || transform == ANATIVEWINDOW_TRANSFORM_ROTATE_270;
    c->layer = frame_layer_create(c->window, turned ? height : width, turned ? width : height, turned ? transform : 0, frame_rate);
    if (c->layer != NULL) c->transform = turned ? transform : 0;
    // The buffer is the X screen's size (turned when stored turned); the compositor scales it to the view.
    if (c->layer == NULL && turned) {
        if (ANativeWindow_setBuffersGeometry(c->window, height, width, WINDOW_FORMAT_RGBX_8888) == 0 &&
            ANativeWindow_setBuffersTransform(c->window, transform) == 0) {
            c->transform = transform;
        } else {
            LOG("buffer transform %d refused; copying as is", transform);
            ANativeWindow_setBuffersTransform(c->window, 0);
        }
    }
    if (c->layer == NULL && c->transform == 0 && ANativeWindow_setBuffersGeometry(c->window, width, height, WINDOW_FORMAT_RGBX_8888) != 0) {
        LOG("setBuffersGeometry %dx%d failed", width, height);
        release(c);
        return 0;
    }
    if (pthread_create(&c->thread, NULL, run, c) != 0) {
        LOG("cannot start the copy thread");
        release(c);
        return 0;
    }
    pthread_mutex_lock(&c->lock);
    while (!c->started) pthread_cond_wait(&c->ready, &c->lock);
    pthread_mutex_unlock(&c->lock);
    LOG("copy started: %dx%d, %d bytes per line, pixels at %lld, buffer transform %d, %s, frame rate %.0f, %s, "
        "frames taken every %.1f ms on average (0: each display frame)", width, height, stride, (long long)offset,
        c->transform, c->layer != NULL ? "child layer" : "window", frame_rate,
        c->pacing != NULL ? "pacing the game" : "not pacing", (double)c->takes.interval_ns / 1e6);
    return (jlong)(intptr_t)c;
}

// The frames whose content changed since the start: the different pictures the copy found.
JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_FrameCopy_nativeChangedFrames(JNIEnv *env, jclass cls, jlong handle) {
    (void)env;
    (void)cls;
    struct copy *c = (struct copy *)(intptr_t)handle;
    return (jlong)__atomic_load_n(&c->changed, __ATOMIC_RELAXED);
}

// The game's presents counted so far (a 32-bit count that wraps), or -1 without the counter.
JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_FrameCopy_nativePresents(JNIEnv *env, jclass cls, jlong handle) {
    (void)env;
    (void)cls;
    struct copy *c = (struct copy *)(intptr_t)handle;
    return c->presents != NULL ? (jlong)presents_now(c) : -1;
}

// The game's presents a display frame showed so far: at most one a display frame.
JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_FrameCopy_nativeShownPresents(JNIEnv *env, jclass cls, jlong handle) {
    (void)env;
    (void)cls;
    struct copy *c = (struct copy *)(intptr_t)handle;
    return (jlong)__atomic_load_n(&c->fresh, __ATOMIC_RELAXED);
}

JNIEXPORT void JNICALL Java_com_pd2launcher_app_FrameCopy_nativeStop(JNIEnv *env, jclass cls, jlong handle) {
    (void)env;
    (void)cls;
    struct copy *c = (struct copy *)(intptr_t)handle;
    if (c == NULL) return;
    atomic_store(&c->stop, true);
    ALooper_wake(c->looper);
    pthread_join(c->thread, NULL);
    set_paced(c, 0);
    ALooper_release(c->looper);
    LOG("copy stopped");
    release(c);
}
