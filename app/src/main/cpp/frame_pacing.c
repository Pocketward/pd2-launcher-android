// The rules of the app's clock, of the direct present's newest frame and of its count of the frames'
// time on the screen (frame_pacing.h).
#include "frame_pacing.h"

int64_t clock_next(int64_t due, int64_t now, int64_t interval) {
    int64_t next = due + interval;
    return next <= now ? now + interval : next;
}

int tick_due(uint32_t tick, uint32_t shown_tick, int shown, int clock_running) {
    return !clock_running || !shown || tick != shown_tick;
}

int keep_newest(struct newest_frame *n, struct sent_frame frame, struct sent_frame *replaced) {
    int had = n->kept;
    if (had) *replaced = n->frame;
    n->frame = frame;
    n->kept = 1;
    return had;
}

int take_newest(struct newest_frame *n, struct sent_frame *frame) {
    if (!n->kept) return 0;
    *frame = n->frame;
    n->kept = 0;
    return 1;
}

int refreshes_on_screen(int64_t on_screen_ns, int64_t refresh_ns) {
    if (refresh_ns <= 0 || on_screen_ns <= 0) return 0;
    int64_t n = (on_screen_ns + refresh_ns / 2) / refresh_ns;
    return n < 1 ? 1 : n > ON_SCREEN_GROUPS ? ON_SCREEN_GROUPS : (int)n;
}
