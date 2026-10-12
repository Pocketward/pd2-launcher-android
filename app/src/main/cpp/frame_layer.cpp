// The Game view's child layer (frame_layer.h). C++ only for <android/surface_control.h>, whose
// declarations take references.
#include "frame_layer.h"

#include <android/hardware_buffer.h>
#include <android/log.h>
#include <android/surface_control.h>
#include <pthread.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define TAG "pd2-gameview"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

// One shown, one queued, one being written.
#define BUFFERS 3
// A buffer the compositor has not given back for this long is taken back anyway.
#define STUCK_NS 1000000000LL

struct frame_layer {
    ASurfaceControl *control;
    AHardwareBuffer *buffers[BUFFERS];
    int buffer_w, buffer_h, stride; // stride in pixels
    int dest_w, dest_h, transform;
    float frame_rate; // asked of the display with the first frame; 0: no vote
    int next, shown; // shown: the buffer the last transaction set, -1 before the first
    int locked; // the buffer frame_layer_lock gave, -1 none
    // Under lock: set by the transaction-complete callbacks on a binder thread.
    pthread_mutex_t lock;
    pthread_cond_t answered;
    int busy[BUFFERS];
    int64_t busy_since[BUFFERS];
    int release_fence[BUFFERS];
    int inflight;
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
    pthread_cond_destroy(&l->answered);
    free(l);
}

frame_layer *frame_layer_create(ANativeWindow *window, int buffer_w, int buffer_h, int transform, float frame_rate) {
    int dest_w = ANativeWindow_getWidth(window), dest_h = ANativeWindow_getHeight(window);
    if (dest_w <= 0 || dest_h <= 0) return nullptr;
    auto *l = (frame_layer *)calloc(1, sizeof(frame_layer));
    if (l == nullptr) return nullptr;
    pthread_mutex_init(&l->lock, nullptr);
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
    AHardwareBuffer_Desc desc = {};
    desc.width = (uint32_t)buffer_w;
    desc.height = (uint32_t)buffer_h;
    desc.layers = 1;
    desc.format = AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM;
    // Written rarely: some gralloc implementations then map them uncached, so a lock and an
    // unlock do not clean and invalidate the CPU caches over the whole buffer.
    desc.usage = AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY | AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
                 AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY;
    for (int i = 0; i < BUFFERS; i++) {
        if (AHardwareBuffer_allocate(&desc, &l->buffers[i]) != 0) {
            LOG("cannot allocate a %dx%d buffer", buffer_w, buffer_h);
            free_layer(l);
            return nullptr;
        }
    }
    AHardwareBuffer_Desc got = {};
    AHardwareBuffer_describe(l->buffers[0], &got);
    l->stride = (int)got.stride;
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
    if (l->busy[i] && now - l->busy_since[i] > STUCK_NS) {
        LOG("buffer %d not given back for %lld ms; taking it back", i, (long long)((now - l->busy_since[i]) / 1000000));
        l->busy[i] = 0;
    }
    int busy = l->busy[i];
    if (!busy) {
        fence = l->release_fence[i];
        l->release_fence[i] = -1;
        l->busy[i] = 1;
        l->busy_since[i] = now;
    }
    pthread_mutex_unlock(&l->lock);
    if (busy) return 0;
    void *bits = nullptr;
    // The lock waits for the release fence (and takes it).
    if (AHardwareBuffer_lock(l->buffers[i], AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY, fence, nullptr, &bits) != 0) {
        LOG("cannot lock buffer %d", i);
        pthread_mutex_lock(&l->lock);
        l->busy[i] = 0;
        pthread_mutex_unlock(&l->lock);
        return 0;
    }
    l->locked = i;
    *pixels = (uint32_t *)bits;
    *stride = l->stride;
    return 1;
}

// The compositor has applied a transaction: the buffer it replaced is free once its fence signals.
static void on_complete(void *context, ASurfaceTransactionStats *stats) {
    auto *done = (completion *)context;
    frame_layer *l = done->layer;
    // Asked for a layer the stats do not list, the platform aborts the process: only when listed,
    // as present_server.cpp asks.
    ASurfaceControl **controls = nullptr;
    size_t count = 0;
    ASurfaceTransactionStats_getASurfaceControls(stats, &controls, &count);
    bool listed = false;
    for (size_t i = 0; i < count && !listed; i++) listed = controls[i] == l->control;
    if (controls != nullptr) ASurfaceTransactionStats_releaseASurfaceControls(controls);
    int fence = listed ? ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats, l->control) : -1;
    pthread_mutex_lock(&l->lock);
    if (done->replaced >= 0) {
        if (l->release_fence[done->replaced] >= 0) close(l->release_fence[done->replaced]);
        l->release_fence[done->replaced] = fence;
        l->busy[done->replaced] = 0;
    } else if (fence >= 0) {
        close(fence);
    }
    l->inflight--;
    pthread_cond_broadcast(&l->answered);
    pthread_mutex_unlock(&l->lock);
    free(done);
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
    if (l->shown < 0) {
        ARect source = {0, 0, l->buffer_w, l->buffer_h};
        ARect destination = {0, 0, l->dest_w, l->dest_h};
        ASurfaceTransaction_setGeometry(t, l->control, source, destination, l->transform);
        ASurfaceTransaction_setBufferTransparency(t, l->control, ASURFACE_TRANSACTION_TRANSPARENCY_OPAQUE);
        ASurfaceTransaction_setVisibility(t, l->control, ASURFACE_TRANSACTION_VISIBILITY_SHOW);
        // A game's explicit vote (Default compatibility): the display runs at this rate if its
        // policy allows (no touch boost against it), not at the rate a heuristic guesses from
        // how often frames come, which never rises above the rate the display already runs at.
        if (l->frame_rate > 0) {
            ASurfaceTransaction_setFrameRate(t, l->control, l->frame_rate, ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT);
        }
    }
    ASurfaceTransaction_setOnComplete(t, done, on_complete);
    pthread_mutex_lock(&l->lock);
    l->inflight++;
    pthread_mutex_unlock(&l->lock);
    ASurfaceTransaction_apply(t);
    ASurfaceTransaction_delete(t);
    l->shown = i;
    l->next = (i + 1) % BUFFERS;
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
    timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 1;
    pthread_mutex_lock(&l->lock);
    while (l->inflight > 0 && pthread_cond_timedwait(&l->answered, &l->lock, &deadline) == 0) {
    }
    int answered = l->inflight == 0;
    pthread_mutex_unlock(&l->lock);
    // A late callback would write into it: then it stays allocated.
    if (answered) {
        free_layer(l);
    } else {
        LOG("the compositor did not answer every frame; the layer's state stays allocated");
    }
}
