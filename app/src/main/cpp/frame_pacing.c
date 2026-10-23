// The rules of the app's clock and of the direct present's newest frame (frame_pacing.h).
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
