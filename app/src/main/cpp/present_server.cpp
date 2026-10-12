// The game's direct present, the app's side of Mesa patch 0004:
// the game's Mesa asks over a Unix socket for one buffer per swapchain image, the app allocates
// linear AHardwareBuffers and sends them, Mesa's GPU blit writes each frame into one, and the app
// shows it on its own child layer of the Game view (above the X screen's copy) by a transaction
// with back pressure, so every buffer is shown and the game is paced by the panel. The layer sits
// where the game's window is on the X screen (x-input-relay says where): the whole view for a
// full-screen game, a windowed game's place otherwise, the copy showing the X screen around it.
// A buffer goes back to Mesa once the compositor has replaced it, with its release fence. Without
// a Game view, or while the game's window is not alone on the X screen (a window over or beside
// it), a frame goes back at once; the word at byte 192 of the present counter's page tells Mesa
// whether to send frames here or to the X screen. A newer swapchain's client waits beside the
// active one until its first frame, which makes it the active one; one whose setup fails or that
// never presents leaves the active one in place. Logs under pd2-present every 5 s while frames
// come.
//
// The protocol (one struct Msg a message, AF_UNIX SOCK_SEQPACKET, a sync_file beside it):
//   Mesa -> app   HELLO   {width, height, count}
//   app -> Mesa   BUFFERS {width, height, count, format}, then count AHardwareBuffer messages
//   Mesa -> app   PRESENT {index}
//   app -> Mesa   RELEASE {index} + release fence
#include "present_server.h"

#include <android/hardware_buffer.h>
#include <android/log.h>
#include <android/native_window_jni.h>
#include <android/surface_control.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <linux/sync_file.h>
#include <poll.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include <atomic>

#define TAG "pd2-present"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

namespace {

constexpr uint32_t MAGIC = 0x50443250u; // "P2DP" in memory
constexpr uint32_t VERSION = 1;
constexpr uint32_t HELLO = 1, BUFFERS = 2, PRESENT = 3, RELEASE = 4;
constexpr int MAX_BUFFERS = 4;
constexpr size_t PAGE = 4096;
constexpr size_t WANTED_WORD = 192 / 4;
constexpr int64_t STATS_NS = 5000000000LL;
constexpr int MAX_FENCES = 1024;

struct Msg {
    uint32_t magic, version, type, index, width, height, count, format;
};

std::atomic<int> showing{0};

int64_t now_ns() {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (int64_t)t.tv_sec * 1000000000LL + t.tv_nsec;
}

// When a signalled sync_file signalled; 0 while pending, -1 on an error.
int64_t fence_time(int fd) {
    sync_fence_info fences[4] = {};
    sync_file_info info = {};
    info.num_fences = 4;
    info.sync_fence_info = (uint64_t)(uintptr_t)fences;
    if (ioctl(fd, SYNC_IOC_FILE_INFO, &info) < 0) return -1;
    if (info.status != 1) return 0;
    int64_t t = 0;
    for (uint32_t i = 0; i < info.num_fences && i < 4; i++) {
        if ((int64_t)fences[i].timestamp_ns > t) t = (int64_t)fences[i].timestamp_ns;
    }
    return t;
}

// One client's buffers: freed when nothing refers to them (the client, the layer, a transaction).
struct BufferSet {
    int refs = 0;
    int fd = -1; // the client's socket; -1 once it has gone
    int count = 0;
    uint32_t width = 0, height = 0;
    AHardwareBuffer *buffers[MAX_BUFFERS] = {};
    bool at_compositor[MAX_BUFFERS] = {}; // sent to the screen, not yet given back
};

void unref(BufferSet *set) {
    if (set == nullptr || --set->refs > 0) return;
    for (int i = 0; i < set->count; i++) {
        if (set->buffers[i] != nullptr) AHardwareBuffer_release(set->buffers[i]);
    }
    delete set;
}

struct Server {
    pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
    pthread_cond_t answered = PTHREAD_COND_INITIALIZER;
    char path[108] = {};
    int listener = -1;
    int wake[2] = {-1, -1};
    pthread_t thread{};
    BufferSet *active = nullptr;
    // A newer swapchain, until its first frame: one whose setup fails (a refused import) or that
    // never presents (a hidden window's) leaves the active one in place.
    BufferSet *pending = nullptr;
    uint32_t *page = nullptr;
    bool covered = true;
    // The game's window as x-input-relay last placed it has no part the layer can show: frames go
    // to the X screen until the relay places it again or another swapchain takes over.
    bool unplaced = false;
    // Where the game's window is on the X screen (its size is the buffers'), and the X screen's
    // size, while it is alone there.
    int32_t window_x = 0, window_y = 0, screen_w = 0, screen_h = 0;
    // The layer, while the Game view has a surface.
    ANativeWindow *window = nullptr;
    ASurfaceControl *control = nullptr;
    int dest_w = 0, dest_h = 0;
    float frame_rate = 0;
    bool configured = false;
    ARect configured_source{}, configured_destination{};
    bool visible = false;
    uint64_t generation = 0;
    int inflight = 0;
    BufferSet *shown_set = nullptr;
    int shown = -1;
    AHardwareBuffer *locked = nullptr;
    // Stats since the last line.
    int64_t last_ns = 0;
    uint64_t presents = 0, back_at_once = 0, completes = 0, shown_frames = 0;
    int fences[MAX_FENCES] = {};
    int nfences = 0;
    int64_t last_present_time = 0, gap_max_ns = 0;
};

struct Completion {
    Server *s;
    uint64_t generation;
    BufferSet *set;
    BufferSet *replaced_set;
    int replaced;
};

// Under lock: Mesa may send frames here (a client, a layer to show them, no window over the game),
// else to the X screen, which the copy shows and the picture reader reads.
void update_wanted(Server *s) {
    if (s->page == nullptr) return;
    uint32_t wanted = s->active != nullptr && s->control != nullptr && !s->covered && !s->unplaced ? 1u : 0u;
    __atomic_store_n(&s->page[WANTED_WORD], wanted, __ATOMIC_RELEASE);
}

void send_release(BufferSet *set, int index, int fence) {
    if (set->fd < 0) return;
    Msg m = {MAGIC, VERSION, RELEASE, (uint32_t)index, set->width, set->height, 0, 0};
    iovec iov = {&m, sizeof(m)};
    union {
        char buf[CMSG_SPACE(sizeof(int))];
        cmsghdr align;
    } control;
    msghdr msg = {};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    if (fence >= 0) {
        msg.msg_control = control.buf;
        msg.msg_controllen = sizeof(control.buf);
        cmsghdr *c = CMSG_FIRSTHDR(&msg);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fence, sizeof(int));
    }
    sendmsg(set->fd, &msg, MSG_NOSIGNAL | MSG_DONTWAIT);
}

// Under lock: buffer [index] of [set] goes back to Mesa, once [fence] (taken) signals.
void give_back(BufferSet *set, int index, int fence) {
    if (set != nullptr && index >= 0 && index < set->count && set->at_compositor[index]) {
        set->at_compositor[index] = false;
        send_release(set, index, fence);
    }
    if (fence >= 0) close(fence);
}

// Under lock: the layer off the screen (the X screen's copy shows again).
void hide(Server *s) __INTRODUCED_IN(29) {
    if (!s->visible || s->control == nullptr) return;
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    ASurfaceTransaction_setVisibility(t, s->control, ASURFACE_TRANSACTION_VISIBILITY_HIDE);
    ASurfaceTransaction_apply(t);
    ASurfaceTransaction_delete(t);
    s->visible = false;
    showing.store(0);
}

void on_complete(void *context, ASurfaceTransactionStats *stats) __INTRODUCED_IN(29) {
    auto *done = (Completion *)context;
    Server *s = done->s;
    pthread_mutex_lock(&s->lock);
    int present_fence = ASurfaceTransactionStats_getPresentFenceFd(stats);
    if (present_fence >= 0) {
        if (s->nfences < MAX_FENCES) s->fences[s->nfences++] = present_fence;
        else close(present_fence);
    }
    int release = -1;
    if (done->generation == s->generation && s->control != nullptr) {
        ASurfaceControl **controls = nullptr;
        size_t count = 0;
        ASurfaceTransactionStats_getASurfaceControls(stats, &controls, &count);
        bool listed = false;
        for (size_t i = 0; i < count && !listed; i++) listed = controls[i] == s->control;
        if (controls != nullptr) ASurfaceTransactionStats_releaseASurfaceControls(controls);
        if (listed) release = ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats, s->control);
    }
    if (done->replaced >= 0) give_back(done->replaced_set, done->replaced, release);
    else if (release >= 0) close(release);
    s->completes++;
    unref(done->set);
    unref(done->replaced_set);
    s->inflight--;
    pthread_cond_broadcast(&s->answered);
    pthread_mutex_unlock(&s->lock);
    free(done);
}

bool same(const ARect &a, const ARect &b) {
    return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

// Under lock: where a buffer of [set] shows: the game's window on the X screen scaled to the
// Game view, the part of it off the X screen cut ([source] in the buffer, [destination] in the
// view). False when nothing of it is on the X screen.
bool place(const Server *s, const BufferSet *set, ARect *source, ARect *destination) {
    int64_t screen_w = s->screen_w > 0 ? s->screen_w : set->width;
    int64_t screen_h = s->screen_h > 0 ? s->screen_h : set->height;
    int64_t x = s->window_x, y = s->window_y;
    int64_t left = x < 0 ? -x : 0, top = y < 0 ? -y : 0;
    int64_t right = (int64_t)set->width < screen_w - x ? (int64_t)set->width : screen_w - x;
    int64_t bottom = (int64_t)set->height < screen_h - y ? (int64_t)set->height : screen_h - y;
    if (right <= left || bottom <= top) return false;
    *source = {(int32_t)left, (int32_t)top, (int32_t)right, (int32_t)bottom};
    // The far edges rounded up: no strip of the X screen's stale copy of the window beside it.
    *destination = {(int32_t)((x + left) * s->dest_w / screen_w), (int32_t)((y + top) * s->dest_h / screen_h),
                    (int32_t)(((x + right) * s->dest_w + screen_w - 1) / screen_w),
                    (int32_t)(((y + bottom) * s->dest_h + screen_h - 1) / screen_h)};
    return destination->right > destination->left && destination->bottom > destination->top;
}

// Under lock: Mesa presents buffer [index] of [set] (its acquire [fence], taken).
void present(Server *s, BufferSet *set, int index, int fence) __INTRODUCED_IN(29) {
    if (index < 0 || index >= set->count || set->at_compositor[index]) {
        if (fence >= 0) close(fence);
        return;
    }
    set->at_compositor[index] = true;
    s->presents++;
    ARect source, destination;
    bool placed = s->control != nullptr && !s->covered && place(s, set, &source, &destination);
    if (!placed) {
        if (s->control != nullptr && !s->covered && !s->unplaced) {
            // The window has no part the layer can show: the X screen shows it.
            s->unplaced = true;
            hide(s);
            update_wanted(s);
        }
        // Nowhere to show it: straight back, so the game never waits for an absent view.
        s->back_at_once++;
        give_back(set, index, fence);
        return;
    }
    auto *done = (Completion *)malloc(sizeof(Completion));
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    ASurfaceTransaction_setBuffer(t, s->control, set->buffers[index], fence);
    if (!s->configured || !same(source, s->configured_source) || !same(destination, s->configured_destination)) {
        // Not turned: the compositor turns the picture to the panel where it has to.
        ASurfaceTransaction_setGeometry(t, s->control, source, destination, 0);
        s->configured_source = source;
        s->configured_destination = destination;
    }
    if (!s->configured) {
        ASurfaceTransaction_setBufferTransparency(t, s->control, ASURFACE_TRANSACTION_TRANSPARENCY_OPAQUE);
        // Above the copy's layer of the X screen.
        ASurfaceTransaction_setZOrder(t, s->control, 1);
        if (s->frame_rate > 0) {
            if (__builtin_available(android 30, *)) {
                ASurfaceTransaction_setFrameRate(t, s->control, s->frame_rate, ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT);
            }
        }
        // Each buffer shown before the next is taken: none dropped, the game paced by the panel.
        if (__builtin_available(android 31, *)) ASurfaceTransaction_setEnableBackPressure(t, s->control, true);
        s->configured = true;
    }
    if (!s->visible) {
        ASurfaceTransaction_setVisibility(t, s->control, ASURFACE_TRANSACTION_VISIBILITY_SHOW);
        s->visible = true;
    }
    // 2: the layer leaves part of the view to the copy (a windowed game's window).
    bool whole = destination.left == 0 && destination.top == 0 && destination.right == s->dest_w && destination.bottom == s->dest_h;
    showing.store(whole ? 1 : 2);
    done->s = s;
    done->generation = s->generation;
    done->set = set;
    set->refs++;
    done->replaced_set = s->shown_set; // the layer's reference moves to the transaction
    done->replaced = s->shown;
    s->shown_set = set;
    set->refs++;
    s->shown = index;
    ASurfaceTransaction_setOnComplete(t, done, on_complete);
    s->inflight++;
    ASurfaceTransaction_apply(t);
    ASurfaceTransaction_delete(t);
}

// Under lock.
void stats(Server *s, int64_t now) {
    for (int i = 0; i < s->nfences; i++) {
        int64_t presented = fence_time(s->fences[i]);
        close(s->fences[i]);
        if (presented <= 0 || presented == s->last_present_time) continue;
        if (s->last_present_time > 0 && presented - s->last_present_time > s->gap_max_ns) {
            s->gap_max_ns = presented - s->last_present_time;
        }
        s->shown_frames++;
        s->last_present_time = presented;
    }
    s->nfences = 0;
    double window = (now - s->last_ns) / 1e9;
    if (s->presents > 0) {
        LOG("%ux%u: %.1f presents/s, %.1f shown/s, longest gap %.1f ms, %.1f/s back at once, %s", s->active ? s->active->width : 0,
            s->active ? s->active->height : 0, s->presents / window, s->shown_frames / window, s->gap_max_ns / 1e6,
            s->back_at_once / window,
            s->visible ? (showing.load() == 2 ? "showing in the game's window" : "showing") : (s->control == nullptr ? "no view" : "the X screen"));
    }
    s->presents = s->back_at_once = s->completes = s->shown_frames = 0;
    s->gap_max_ns = 0;
    s->last_ns = now;
}

ssize_t receive(int fd, void *data, size_t cap, int *fds, int max, int *nfds) {
    iovec iov = {data, cap};
    union {
        char buf[CMSG_SPACE(8 * sizeof(int))];
        cmsghdr align;
    } control;
    msghdr msg = {};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.buf;
    msg.msg_controllen = sizeof(control.buf);
    ssize_t n;
    do n = recvmsg(fd, &msg, MSG_CMSG_CLOEXEC); while (n < 0 && errno == EINTR);
    *nfds = 0;
    for (cmsghdr *c = CMSG_FIRSTHDR(&msg); c != nullptr; c = CMSG_NXTHDR(&msg, c)) {
        if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS) continue;
        int count = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
        int *got = (int *)CMSG_DATA(c);
        for (int i = 0; i < count; i++) {
            if (*nfds < max) fds[(*nfds)++] = got[i];
            else close(got[i]);
        }
    }
    return n;
}

// A new client: its HELLO, its buffers allocated and sent. Null (and the socket closed) on failure.
BufferSet *welcome(int fd) {
    timeval timeout = {1, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    Msg m;
    int fds[4], nfds;
    ssize_t n = receive(fd, &m, sizeof(m), fds, 4, &nfds);
    for (int i = 0; i < nfds; i++) close(fds[i]);
    if (n != sizeof(m) || m.magic != MAGIC || m.type != HELLO || m.count < 2 || m.count > MAX_BUFFERS || m.width == 0 ||
        m.height == 0 || m.width > 8192 || m.height > 8192) {
        LOG("a client without a proper HELLO");
        close(fd);
        return nullptr;
    }
    auto *set = new BufferSet();
    set->fd = fd;
    set->count = (int)m.count;
    set->width = m.width;
    set->height = m.height;
    set->refs = 1; // the client's
    AHardwareBuffer_Desc desc = {};
    desc.width = m.width;
    desc.height = m.height;
    desc.layers = 1;
    desc.format = AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM;
    // Linear on Qualcomm's and Google's grallocs: a CPU usage bit rules out UBWC and AFBC (and the
    // Game view's picture reader may look at a few pixels).
    desc.usage = AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT | AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE |
                 AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY | AHARDWAREBUFFER_USAGE_CPU_READ_RARELY;
    for (int i = 0; i < set->count; i++) {
        if (AHardwareBuffer_allocate(&desc, &set->buffers[i]) != 0) {
            LOG("cannot allocate a %ux%u buffer", m.width, m.height);
            close(fd);
            set->fd = -1;
            unref(set);
            return nullptr;
        }
    }
    Msg reply = {MAGIC, VERSION, BUFFERS, 0, set->width, set->height, (uint32_t)set->count, AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM};
    bool sent = send(fd, &reply, sizeof(reply), MSG_NOSIGNAL) == sizeof(reply);
    for (int i = 0; sent && i < set->count; i++) sent = AHardwareBuffer_sendHandleToUnixSocket(set->buffers[i], fd) == 0;
    if (!sent) {
        LOG("cannot send the buffers: %s", strerror(errno));
        close(fd);
        set->fd = -1;
        unref(set);
        return nullptr;
    }
    timeout = {0, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    AHardwareBuffer_Desc got = {};
    AHardwareBuffer_describe(set->buffers[0], &got);
    LOG("the game's swapchain: %d buffers of %ux%u (stride %u)", set->count, set->width, set->height, got.stride);
    return set;
}

// Under lock: [set] becomes the swapchain whose frames show.
void activate(Server *s, BufferSet *set) __INTRODUCED_IN(29) {
    s->active = set;
    s->unplaced = false;
    update_wanted(s);
}

// Under lock: the client has gone (or another replaced it). A pending newer one takes over from
// an active one that went first.
void farewell(Server *s, BufferSet *set) __INTRODUCED_IN(29) {
    if (set == nullptr) return;
    if (set->fd >= 0) {
        close(set->fd);
        set->fd = -1;
    }
    if (s->pending == set) s->pending = nullptr;
    if (s->active == set) {
        s->active = nullptr;
        hide(s);
        BufferSet *next = s->pending;
        s->pending = nullptr;
        activate(s, next);
    }
    unref(set); // the client's reference
}

// Under lock: a message (or the end) from client [set].
void message(Server *s, BufferSet *set, ssize_t n, const Msg &m, int *fences, int nfences) __INTRODUCED_IN(29) {
    bool is_present = n == sizeof(m) && m.magic == MAGIC && m.type == PRESENT;
    if (n <= 0) {
        LOG("%s", set == s->active ? "the game's swapchain has gone" : "a newer swapchain has gone before its first frame");
        farewell(s, set);
    } else if (is_present && set == s->pending) {
        // Its first frame: it replaces the older one, whose Mesa goes back to the X screen.
        LOG("the newer swapchain presents: it replaces the older");
        s->pending = nullptr;
        farewell(s, s->active);
        activate(s, set);
    }
    if (n > 0 && is_present && set == s->active) {
        for (int i = 1; i < nfences; i++) close(fences[i]);
        present(s, set, (int)m.index, nfences > 0 ? fences[0] : -1);
    } else {
        for (int i = 0; i < nfences; i++) close(fences[i]);
    }
}

void serve(Server *s) __INTRODUCED_IN(29) {
    for (;;) {
        pthread_mutex_lock(&s->lock);
        BufferSet *clients[2] = {s->active, s->pending};
        int fds_of[2] = {clients[0] != nullptr ? clients[0]->fd : -1, clients[1] != nullptr ? clients[1]->fd : -1};
        pthread_mutex_unlock(&s->lock);
        // A negative descriptor is not polled.
        pollfd fds[4] = {{s->wake[0], POLLIN, 0}, {s->listener, POLLIN, 0}, {fds_of[0], POLLIN, 0}, {fds_of[1], POLLIN, 0}};
        if (poll(fds, 4, -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (fds[0].revents) break;
        if (fds[1].revents & POLLIN) {
            int fd = accept4(s->listener, nullptr, nullptr, SOCK_CLOEXEC);
            BufferSet *set = fd >= 0 ? welcome(fd) : nullptr;
            if (set != nullptr) {
                pthread_mutex_lock(&s->lock);
                if (s->active == nullptr) {
                    activate(s, set);
                } else {
                    // The active swapchain shows until the new one's first frame; a newer one
                    // still waiting is dropped (its Mesa goes back to the X screen).
                    farewell(s, s->pending);
                    s->pending = set;
                }
                pthread_mutex_unlock(&s->lock);
                // Descriptors may have closed: poll again.
                continue;
            }
        }
        for (int c = 0; c < 2; c++) {
            if (fds_of[c] < 0 || !(fds[2 + c].revents & (POLLIN | POLLHUP | POLLERR))) continue;
            Msg m;
            int fences[4], nfences;
            ssize_t n = receive(fds_of[c], &m, sizeof(m), fences, 4, &nfences);
            pthread_mutex_lock(&s->lock);
            BufferSet *set = s->active != nullptr && s->active->fd == fds_of[c] ? s->active
                             : s->pending != nullptr && s->pending->fd == fds_of[c] ? s->pending
                                                                                     : nullptr;
            if (set == nullptr) {
                for (int i = 0; i < nfences; i++) close(fences[i]);
            } else {
                message(s, set, n, m, fences, nfences);
            }
            int64_t now = now_ns();
            if (now - s->last_ns >= STATS_NS) stats(s, now);
            pthread_mutex_unlock(&s->lock);
        }
    }
}

void *run(void *data) {
    if (__builtin_available(android 29, *)) serve((Server *)data);
    return nullptr;
}

// Under lock: the layer goes (the Game view's surface is going); every buffer it held goes back.
void drop_layer(Server *s) __INTRODUCED_IN(29) {
    if (s->control == nullptr) return;
    ASurfaceTransaction *t = ASurfaceTransaction_create();
    ASurfaceTransaction_reparent(t, s->control, nullptr);
    ASurfaceTransaction_apply(t);
    ASurfaceTransaction_delete(t);
    s->generation++;
    timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 1;
    while (s->inflight > 0 && pthread_cond_timedwait(&s->answered, &s->lock, &deadline) == 0) {
    }
    if (s->shown >= 0) give_back(s->shown_set, s->shown, -1);
    unref(s->shown_set);
    s->shown_set = nullptr;
    s->shown = -1;
    ASurfaceControl_release(s->control);
    s->control = nullptr;
    ANativeWindow_release(s->window);
    s->window = nullptr;
    s->configured = false;
    s->visible = false;
    showing.store(0);
    update_wanted(s);
}

Server *start(JNIEnv *env, jstring socket_path, jstring counter_path) {
    auto *s = new Server();
    const char *chars = env->GetStringUTFChars(socket_path, nullptr);
    snprintf(s->path, sizeof(s->path), "%s", chars);
    env->ReleaseStringUTFChars(socket_path, chars);
    if (counter_path != nullptr) {
        const char *file = env->GetStringUTFChars(counter_path, nullptr);
        int fd = open(file, O_RDWR | O_CLOEXEC);
        struct stat st;
        if (fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= (off_t)PAGE) {
            void *map = mmap(nullptr, PAGE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
            if (map != MAP_FAILED) s->page = (uint32_t *)map;
        }
        if (fd >= 0) close(fd);
        env->ReleaseStringUTFChars(counter_path, file);
    }
    if (s->page == nullptr) {
        LOG("no present counter page: the X screen only");
        delete s;
        return nullptr;
    }
    update_wanted(s);
    unlink(s->path);
    s->listener = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    sockaddr_un addr = {};
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", s->path);
    if (s->listener < 0 || bind(s->listener, (sockaddr *)&addr, sizeof(addr)) != 0 || listen(s->listener, 4) != 0 ||
        pipe2(s->wake, O_CLOEXEC) != 0) {
        LOG("cannot listen on %s: %s", s->path, strerror(errno));
        if (s->listener >= 0) close(s->listener);
        munmap(s->page, PAGE);
        delete s;
        return nullptr;
    }
    s->last_ns = now_ns();
    if (pthread_create(&s->thread, nullptr, run, s) != 0) {
        LOG("cannot start the server thread");
        close(s->listener);
        close(s->wake[0]);
        close(s->wake[1]);
        unlink(s->path);
        munmap(s->page, PAGE);
        delete s;
        return nullptr;
    }
    LOG("listening on %s", s->path);
    return s;
}

} // namespace

extern "C" int present_server_showing(void) {
    return showing.load(std::memory_order_relaxed);
}

extern "C" JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_PresentServer_nativeStart(
    JNIEnv *env, jclass, jstring socket_path, jstring counter_path) {
    if (__builtin_available(android 29, *)) return (jlong)(intptr_t)start(env, socket_path, counter_path);
    return 0;
}

namespace {

void attach(Server *s, JNIEnv *env, jobject surface, jfloat frame_rate) __INTRODUCED_IN(29) {
    ANativeWindow *window = ANativeWindow_fromSurface(env, surface);
    if (window == nullptr) return;
    pthread_mutex_lock(&s->lock);
    drop_layer(s);
    s->window = window;
    s->dest_w = ANativeWindow_getWidth(window);
    s->dest_h = ANativeWindow_getHeight(window);
    s->frame_rate = frame_rate;
    s->control = ASurfaceControl_createFromWindow(window, "pd2-game-direct");
    if (s->control == nullptr) {
        ANativeWindow_release(window);
        s->window = nullptr;
    }
    update_wanted(s);
    pthread_mutex_unlock(&s->lock);
    LOG("the Game view: %dx%d, frame rate %.0f", s->dest_w, s->dest_h, frame_rate);
}

void detach(Server *s) __INTRODUCED_IN(29) {
    pthread_mutex_lock(&s->lock);
    drop_layer(s);
    pthread_mutex_unlock(&s->lock);
}

void set_window(Server *s, bool covered, int32_t x, int32_t y, int32_t w, int32_t h, int32_t screen_w, int32_t screen_h)
    __INTRODUCED_IN(29) {
    pthread_mutex_lock(&s->lock);
    s->covered = covered;
    s->window_x = x;
    s->window_y = y;
    s->screen_w = screen_w;
    s->screen_h = screen_h;
    s->unplaced = false;
    update_wanted(s);
    if (covered) hide(s);
    pthread_mutex_unlock(&s->lock);
    if (covered) {
        LOG("not the game's window alone on the X screen: the X screen");
    } else {
        LOG("the game's window alone: %dx%d at %d,%d on the %dx%d X screen", w, h, x, y, screen_w, screen_h);
    }
}

void stop(Server *s) __INTRODUCED_IN(29) {
    char byte = 1;
    if (write(s->wake[1], &byte, 1) != 1) LOG("cannot wake the server");
    pthread_join(s->thread, nullptr);
    pthread_mutex_lock(&s->lock);
    farewell(s, s->pending);
    farewell(s, s->active);
    drop_layer(s);
    bool answered = s->inflight == 0;
    if (s->page != nullptr) __atomic_store_n(&s->page[WANTED_WORD], 0u, __ATOMIC_RELEASE);
    for (int i = 0; i < s->nfences; i++) close(s->fences[i]);
    s->nfences = 0;
    pthread_mutex_unlock(&s->lock);
    close(s->listener);
    close(s->wake[0]);
    close(s->wake[1]);
    unlink(s->path);
    munmap(s->page, PAGE);
    // A late callback would find it: then it stays allocated.
    if (answered) delete s;
    LOG("stopped");
}

} // namespace

// The Game view's surface (a child layer goes on it), asking the display for [frame_rate].
extern "C" JNIEXPORT void JNICALL Java_com_pd2launcher_app_PresentServer_nativeAttach(
    JNIEnv *env, jclass, jlong handle, jobject surface, jfloat frame_rate) {
    if (__builtin_available(android 29, *)) attach((Server *)(intptr_t)handle, env, surface, frame_rate);
}

extern "C" JNIEXPORT void JNICALL Java_com_pd2launcher_app_PresentServer_nativeDetach(JNIEnv *, jclass, jlong handle) {
    if (__builtin_available(android 29, *)) detach((Server *)(intptr_t)handle);
}

// x-input-relay's word: [covered], the game's window is not alone on the X screen; else it is at
// [x], [y], [w] x [h] on the [screen_w] x [screen_h] X screen.
extern "C" JNIEXPORT void JNICALL Java_com_pd2launcher_app_PresentServer_nativeSetWindow(
    JNIEnv *, jclass, jlong handle, jboolean covered, jint x, jint y, jint w, jint h, jint screen_w, jint screen_h) {
    if (__builtin_available(android 29, *)) set_window((Server *)(intptr_t)handle, covered, x, y, w, h, screen_w, screen_h);
}

// The buffer on the screen, locked for reading: its pixels (R8G8B8A8 words), and [size] = width,
// height, bytes a row, then where its top-left corner is on the X screen (x, y) and the X
// screen's width and height; null while none shows. nativeUnlockShown unlocks it.
extern "C" JNIEXPORT jobject JNICALL Java_com_pd2launcher_app_PresentServer_nativeLockShown(
    JNIEnv *env, jclass, jlong handle, jintArray size) {
    auto *s = (Server *)(intptr_t)handle;
    pthread_mutex_lock(&s->lock);
    AHardwareBuffer *buffer = s->visible && s->shown_set != nullptr ? s->shown_set->buffers[s->shown] : nullptr;
    if (buffer != nullptr) AHardwareBuffer_acquire(buffer);
    jint place[4] = {s->window_x, s->window_y, s->screen_w, s->screen_h};
    pthread_mutex_unlock(&s->lock);
    if (buffer == nullptr) return nullptr;
    void *pixels = nullptr;
    if (AHardwareBuffer_lock(buffer, AHARDWAREBUFFER_USAGE_CPU_READ_RARELY, -1, nullptr, &pixels) != 0) {
        AHardwareBuffer_release(buffer);
        return nullptr;
    }
    AHardwareBuffer_Desc desc = {};
    AHardwareBuffer_describe(buffer, &desc);
    jint values[7] = {(jint)desc.width, (jint)desc.height, (jint)(desc.stride * 4), place[0], place[1],
                      place[2] > 0 ? place[2] : (jint)desc.width, place[3] > 0 ? place[3] : (jint)desc.height};
    env->SetIntArrayRegion(size, 0, 7, values);
    s->locked = buffer;
    return env->NewDirectByteBuffer(pixels, (jlong)desc.stride * 4 * desc.height);
}

extern "C" JNIEXPORT void JNICALL Java_com_pd2launcher_app_PresentServer_nativeUnlockShown(JNIEnv *, jclass, jlong handle) {
    auto *s = (Server *)(intptr_t)handle;
    if (s->locked == nullptr) return;
    AHardwareBuffer_unlock(s->locked, nullptr);
    AHardwareBuffer_release(s->locked);
    s->locked = nullptr;
}

extern "C" JNIEXPORT void JNICALL Java_com_pd2launcher_app_PresentServer_nativeStop(JNIEnv *, jclass, jlong handle) {
    auto *s = (Server *)(intptr_t)handle;
    if (s == nullptr) return;
    if (__builtin_available(android 29, *)) stop(s);
}
