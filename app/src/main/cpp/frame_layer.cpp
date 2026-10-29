// The Game view's child layer (frame_layer.h). C++ only for <android/surface_control.h>, whose
// declarations take references.
#include "frame_layer.h"

#include <android/hardware_buffer.h>
#include <android/log.h>
#include <android/surface_control.h>
#include <poll.h>
#include <pthread.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define TAG "pd2-gameview"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

// One shown, one queued, one being written.
#define BUFFERS 3
// The copy rests this long without a break before the buffers off the screen go
// (frame_layer_rest): a short return to the copy keeps them.
#define REST_RELEASE_NS 5000000000LL
// A buffer the allocator refused is asked for again after this long at the soonest.
#define ALLOCATE_AGAIN_NS 100000000LL
// The compositor holds the next buffer this long: one line in the log (nothing else changes).
#define HELD_LOG_NS 1000000000LL

struct frame_layer {
    // Under lock once frame_layer_destroy runs: it lets the layer go before it waits (NULL).
    ASurfaceControl *control;
    AHardwareBuffer_Desc desc;
    // NULL: not allocated yet, or released while the copy rested. Each is allocated when it is
    // first locked.
    AHardwareBuffer *buffers[BUFFERS];
    int stride[BUFFERS]; // in pixels
    int buffer_w, buffer_h;
    int dest_w, dest_h, transform; // dest_w, dest_h under lock (frame_layer_resize)
    int geometry_due; // under lock: the next frame sets the geometry again (frame_layer_resize)
    float frame_rate; // asked of the display with the first frame; 0: no vote
    int next, shown; // shown: the buffer the last transaction set, -1 before the first; written under lock
    int locked; // the buffer frame_layer_lock gave, -1 none
    int refusals; // allocations refused in a row (the first is logged)
    int64_t refused_ns; // the last refusal
    // frame_layer_lock (the copy's thread): since when the compositor holds the next buffer (0:
    // it does not), and whether that wait is logged (HELD_LOG_NS).
    int64_t held_since;
    int held_logged;
    // frame_layer_rest: whether the copy rests and since when, and whether every buffer that can
    // go has gone. [redraw]: a buffer went, allocation failed, or a busy slot prevented a post
    // since the last frame shown. The copy shows the X screen again, changed or not.
    int resting;
    int64_t rest_since;
    int rest_done;
    int redraw;
    // Held from reading the geometry to applying its transaction, by frame_layer_post (the copy's
    // thread) and frame_layer_resize (the main thread): their transactions apply in the order they
    // read it, so the newest size is the last one set.
    pthread_mutex_t geometry_lock;
    // Under lock: set by the transaction-complete callbacks on a binder thread.
    pthread_mutex_t lock;
    pthread_cond_t answered;
    int busy[BUFFERS];
    int release_fence[BUFFERS];
    int inflight;
    // frame_layer_destroy stopped waiting for the compositor: the last callback frees the layer.
    int abandoned;
};

struct completion {
    frame_layer *layer;
    int replaced; // the buffer this transaction replaced on the screen, -1 none
};

static int64_t now_ns() {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

static void free_layer(frame_layer *l) {
    for (int i = 0; i < BUFFERS; i++) {
        if (l->buffers[i] != nullptr) AHardwareBuffer_release(l->buffers[i]);
        if (l->release_fence[i] >= 0) close(l->release_fence[i]);
    }
    if (l->control != nullptr) ASurfaceControl_release(l->control);
    pthread_mutex_destroy(&l->lock);
    pthread_mutex_destroy(&l->geometry_lock);
    pthread_cond_destroy(&l->answered);
    free(l);
}

// Buffer [i] allocated at [now]; false (the first of a run logged) when the allocator refuses it.
static bool allocate(frame_layer *l, int i, int64_t now) {
    if (AHardwareBuffer_allocate(&l->desc, &l->buffers[i]) != 0) {
        l->buffers[i] = nullptr;
        if (l->refusals++ == 0) LOG("cannot allocate a %dx%d buffer", l->buffer_w, l->buffer_h);
        l->refused_ns = now;
        return false;
    }
    if (l->refusals > 0) LOG("buffer %d allocated after %d refusals", i, l->refusals);
    l->refusals = 0;
    AHardwareBuffer_Desc got = {};
    AHardwareBuffer_describe(l->buffers[i], &got);
    l->stride[i] = (int)got.stride;
    return true;
}

void frame_layer_set_rate(frame_layer *l, float frame_rate) {
    pthread_mutex_lock(&l->lock);
    l->frame_rate = frame_rate;
    pthread_mutex_unlock(&l->lock);
    if (frame_rate <= 0) return;
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    ASurfaceTransaction_setFrameRate(t, l->control, frame_rate, ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT);
    ASurfaceTransaction_apply(t);
    ASurfaceTransaction_delete(t);
}

frame_layer *frame_layer_create(ANativeWindow *window, int buffer_w, int buffer_h, int transform, float frame_rate) {
    int dest_w = ANativeWindow_getWidth(window), dest_h = ANativeWindow_getHeight(window);
    if (dest_w <= 0 || dest_h <= 0) return nullptr;
    auto *l = (frame_layer *)calloc(1, sizeof(frame_layer));
    if (l == nullptr) return nullptr;
    pthread_mutex_init(&l->lock, nullptr);
    pthread_mutex_init(&l->geometry_lock, nullptr);
    pthread_cond_init(&l->answered, nullptr);
    for (int i = 0; i < BUFFERS; i++) l->release_fence[i] = -1;
    l->buffer_w = buffer_w;
    l->buffer_h = buffer_h;
    l->dest_w = dest_w;
    l->dest_h = dest_h;
    l->transform = transform;
    l->frame_rate = frame_rate;
    l->shown = -1;
    l->locked = -1;
    l->desc.width = (uint32_t)buffer_w;
    l->desc.height = (uint32_t)buffer_h;
    l->desc.layers = 1;
    l->desc.format = AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM;
    // Written rarely: some gralloc implementations then map them uncached, so a lock and an
    // unlock do not clean and invalidate the CPU caches over the whole buffer.
    l->desc.usage = AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY | AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
                    AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY;
    // The first buffer shows that the allocator gives them (else the copy goes through the
    // window); the others come when the copy first needs them.
    if (!allocate(l, 0, now_ns())) {
        free_layer(l);
        return nullptr;
    }
    l->control = ASurfaceControl_createFromWindow(window, "pd2-game");
    if (l->control == nullptr) {
        free_layer(l);
        return nullptr;
    }
    return l;
}

int frame_layer_lock(frame_layer *l, uint32_t **pixels, int *stride) {
    int i = l->next;
    int64_t now = now_ns();
    int fence = -1;
    pthread_mutex_lock(&l->lock);
    int busy = l->busy[i];
    if (!busy) {
        fence = l->release_fence[i];
        l->release_fence[i] = -1;
        l->busy[i] = 1;
    }
    pthread_mutex_unlock(&l->lock);
    if (busy) {
        l->redraw = 1;
        if (l->held_since == 0) {
            l->held_since = now;
        } else if (!l->held_logged && now - l->held_since >= HELD_LOG_NS) {
            LOG("buffer %d still held by the compositor after %lld ms; no frame until it is given back", i,
                (long long)((now - l->held_since) / 1000000));
            l->held_logged = 1;
        }
        return 0;
    }
    if (l->held_logged) LOG("buffer %d given back after %lld ms", i, (long long)((now - l->held_since) / 1000000));
    l->held_since = 0;
    l->held_logged = 0;
    void *bits = nullptr;
    // A buffer not allocated yet has no fence: it was never on the screen, or went once it was
    // off the screen and its fence had signalled. One the allocator refuses is asked for again
    // on a later display frame (redraw), not before ALLOCATE_AGAIN_NS.
    bool missing = false;
    if (l->buffers[i] == nullptr) {
        missing = (l->refusals > 0 && now - l->refused_ns < ALLOCATE_AGAIN_NS) || !allocate(l, i, now);
        if (missing) l->redraw = 1;
        else LOG("buffer %d allocated", i);
    }
    // The lock waits for the release fence (and takes it).
    if (missing || AHardwareBuffer_lock(l->buffers[i], AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY, fence, nullptr, &bits) != 0) {
        if (missing) {
            if (fence >= 0) close(fence);
        } else {
            LOG("cannot lock buffer %d", i);
        }
        pthread_mutex_lock(&l->lock);
        l->busy[i] = 0;
        pthread_mutex_unlock(&l->lock);
        return missing ? -1 : 0;
    }
    l->locked = i;
    *pixels = (uint32_t *)bits;
    *stride = l->stride[i];
    return 1;
}

// The compositor has applied a transaction: the buffer it replaced is free once its fence signals.
static void on_complete(void *context, ASurfaceTransactionStats *stats) {
    auto *done = (completion *)context;
    frame_layer *l = done->layer;
    pthread_mutex_lock(&l->lock);
    int fence = -1;
    // Asked for a layer the stats do not list, the platform aborts the process: only when listed,
    // as present_server.cpp asks. Once frame_layer_destroy has let the layer go, its fences no
    // longer count (the lock keeps it from going while it is asked for here).
    if (l->control != nullptr) {
        ASurfaceControl **controls = nullptr;
        size_t count = 0;
        ASurfaceTransactionStats_getASurfaceControls(stats, &controls, &count);
        bool listed = false;
        for (size_t i = 0; i < count && !listed; i++) listed = controls[i] == l->control;
        if (controls != nullptr) ASurfaceTransactionStats_releaseASurfaceControls(controls);
        if (listed) fence = ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats, l->control);
    }
    if (done->replaced >= 0) {
        if (l->release_fence[done->replaced] >= 0) close(l->release_fence[done->replaced]);
        l->release_fence[done->replaced] = fence;
        l->busy[done->replaced] = 0;
    } else if (fence >= 0) {
        close(fence);
    }
    l->inflight--;
    // The last answer to a layer frame_layer_destroy left: no thread uses it any more.
    bool orphan = l->abandoned && l->inflight == 0;
    pthread_cond_broadcast(&l->answered);
    pthread_mutex_unlock(&l->lock);
    free(done);
    if (orphan) {
        free_layer(l);
        LOG("the compositor answered every frame late; the layer is freed");
    }
}

void frame_layer_post(frame_layer *l) {
    int i = l->locked;
    if (i < 0) return;
    l->locked = -1;
    int ready = -1;
    AHardwareBuffer_unlock(l->buffers[i], &ready);
    auto *done = (completion *)malloc(sizeof(completion));
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    if (done == nullptr || t == nullptr) {
        if (ready >= 0) close(ready);
        free(done);
        if (t != nullptr) ASurfaceTransaction_delete(t);
        pthread_mutex_lock(&l->lock);
        l->busy[i] = 0;
        pthread_mutex_unlock(&l->lock);
        return;
    }
    done->layer = l;
    done->replaced = l->shown;
    ASurfaceTransaction_setBuffer(t, l->control, l->buffers[i], ready);
    pthread_mutex_lock(&l->geometry_lock);
    pthread_mutex_lock(&l->lock);
    bool geometry = l->shown < 0 || l->geometry_due;
    l->geometry_due = 0;
    ARect destination = {0, 0, l->dest_w, l->dest_h};
    pthread_mutex_unlock(&l->lock);
    if (geometry) {
        ARect source = {0, 0, l->buffer_w, l->buffer_h};
        ASurfaceTransaction_setGeometry(t, l->control, source, destination, l->transform);
    }
    if (l->shown < 0) {
        ASurfaceTransaction_setBufferTransparency(t, l->control, ASURFACE_TRANSACTION_TRANSPARENCY_OPAQUE);
        ASurfaceTransaction_setVisibility(t, l->control, ASURFACE_TRANSACTION_VISIBILITY_SHOW);
        // A game's explicit vote (Default compatibility): the display runs at this rate if its
        // policy allows (no touch boost against it), not at the rate a heuristic guesses from
        // how often frames come, which never rises above the rate the display already runs at.
        pthread_mutex_lock(&l->lock);
        float rate = l->frame_rate;
        pthread_mutex_unlock(&l->lock);
        if (rate > 0) ASurfaceTransaction_setFrameRate(t, l->control, rate, ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT);
    }
    ASurfaceTransaction_setOnComplete(t, done, on_complete);
    pthread_mutex_lock(&l->lock);
    l->inflight++;
    l->shown = i;
    pthread_mutex_unlock(&l->lock);
    ASurfaceTransaction_apply(t);
    pthread_mutex_unlock(&l->geometry_lock);
    ASurfaceTransaction_delete(t);
    l->next = (i + 1) % BUFFERS;
    l->redraw = 0;
}

void frame_layer_resize(frame_layer *l, int dest_w, int dest_h) {
    if (dest_w <= 0 || dest_h <= 0) return;
    // After a frame that read the older size: its transaction applies first (geometry_lock).
    pthread_mutex_lock(&l->geometry_lock);
    pthread_mutex_lock(&l->lock);
    l->dest_w = dest_w;
    l->dest_h = dest_h;
    l->geometry_due = 1;
    bool shown = l->shown >= 0;
    pthread_mutex_unlock(&l->lock);
    // Before the first frame, that frame sets it.
    ASurfaceTransaction *t = shown ? ASurfaceTransaction_create() : nullptr;
    if (t != nullptr) {
        ARect source = {0, 0, l->buffer_w, l->buffer_h};
        ARect destination = {0, 0, dest_w, dest_h};
        ASurfaceTransaction_setGeometry(t, l->control, source, destination, l->transform);
        ASurfaceTransaction_apply(t);
        ASurfaceTransaction_delete(t);
    }
    pthread_mutex_unlock(&l->geometry_lock);
}

// Whether the release fence [fd] has signalled (-1: there is none).
static bool signalled(int fd) {
    if (fd < 0) return true;
    pollfd p = {fd, POLLIN, 0};
    return poll(&p, 1, 0) > 0;
}

// The buffers that are neither on the screen, nor queued or read by the compositor (their
// release fences signalled), go. True when none but the one on the screen is left.
static bool release_unused(frame_layer *l) {
    AHardwareBuffer *gone[BUFFERS];
    int count = 0, kept = 0;
    bool left = false;
    pthread_mutex_lock(&l->lock);
    for (int i = 0; i < BUFFERS; i++) {
        if (l->buffers[i] == nullptr) continue;
        if (i == l->shown || i == l->locked || l->busy[i] || !signalled(l->release_fence[i])) {
            kept++;
            left = left || i != l->shown;
            continue;
        }
        if (l->release_fence[i] >= 0) close(l->release_fence[i]);
        l->release_fence[i] = -1;
        gone[count++] = l->buffers[i];
        l->buffers[i] = nullptr;
    }
    pthread_mutex_unlock(&l->lock);
    for (int i = 0; i < count; i++) AHardwareBuffer_release(gone[i]);
    if (count > 0) {
        l->redraw = 1;
        LOG("the copy rests: %d buffers released, %d kept", count, kept);
    }
    return !left;
}

int frame_layer_rest(frame_layer *l, int64_t frame_ns, int resting) {
    if (!resting) {
        l->resting = 0;
        l->rest_done = 0;
    } else if (!l->resting) {
        l->resting = 1;
        l->rest_since = frame_ns;
    } else if (!l->rest_done && frame_ns - l->rest_since >= REST_RELEASE_NS) {
        l->rest_done = release_unused(l);
    }
    return l->redraw;
}

void frame_layer_destroy(frame_layer *l) {
    if (l == nullptr) return;
    if (l->locked >= 0) {
        AHardwareBuffer_unlock(l->buffers[l->locked], nullptr);
        l->locked = -1;
    }
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    if (t != nullptr) {
        ASurfaceTransaction_reparent(t, l->control, nullptr);
        ASurfaceTransaction_apply(t);
        ASurfaceTransaction_delete(t);
    }
    // The layer goes before the wait, as present_server.cpp's: a frame still queued for it when it
    // left the screen is answered only once it has gone.
    pthread_mutex_lock(&l->lock);
    ASurfaceControl *control = l->control;
    l->control = nullptr;
    pthread_mutex_unlock(&l->lock);
    ASurfaceControl_release(control);
    timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 1;
    pthread_mutex_lock(&l->lock);
    while (l->inflight > 0 && pthread_cond_timedwait(&l->answered, &l->lock, &deadline) == 0) {
    }
    int answered = l->inflight == 0;
    // A late callback would write into it: the last one frees it (on_complete). Nothing here
    // touches the layer after the unlock.
    if (!answered) l->abandoned = 1;
    pthread_mutex_unlock(&l->lock);
    if (answered) {
        free_layer(l);
    } else {
        LOG("the compositor did not answer every frame; the last answer frees the layer");
    }
}
