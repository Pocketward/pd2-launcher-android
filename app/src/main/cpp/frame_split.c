// The split of each shown frame's time (frame_split.h).
#include "frame_split.h"
#include "split_stats.h"
#include <string.h>

void frame_split_reset(struct frame_split *s) {
    s->since = s->last = s->last_presented = 0;
    s->frames = 0;
}

// The window's summary into [out]; the next window starts empty.
static void summarize(struct frame_split *s, struct frame_summary *out) {
    memset(out, 0, sizeof(*out));
    out->span_ns = s->last - s->since;
    out->frames = s->frames;
    for (int k = 0; k < FRAME_PARTS; k++) {
        if (s->frames > 0) split_ranks(s->us[k], s->frames, &out->median_us[k], &out->p95_us[k]);
        else out->median_us[k] = out->p95_us[k] = -1;
    }
    s->since = s->last = 0;
    s->frames = 0;
}

int frame_split_shown(struct frame_split *s, const struct split_frame *frame, int64_t window_ns, struct frame_summary *ended) {
    if (frame->acquired == 0) return 0;
    int done = 0;
    if (s->frames > 0 && (frame->acquired - s->since >= window_ns || s->frames == FRAME_SAMPLES)) {
        summarize(s, ended);
        done = 1;
    }
    if (s->frames == 0) s->since = frame->acquired;
    s->last = frame->acquired;
    int n = s->frames++;
    s->us[FRAME_PRESENT_ACQUIRE][n] = s->last_presented > 0 && frame->acquired >= s->last_presented
        ? split_micros(s->last_presented, frame->acquired) : -1;
    s->us[FRAME_ACQUIRE_PRESENT][n] = split_micros(frame->acquired, frame->presented);
    s->us[FRAME_PRESENT_GPU][n] = frame->gpu_done > 0 ? split_micros(frame->presented, frame->gpu_done) : -1;
    s->us[FRAME_PRESENT_CLOCK][n] = frame->clock > 0 ? split_micros(frame->presented, frame->clock) : -1;
    s->us[FRAME_CLOCK_APPLY][n] = frame->applied > 0 ? split_micros(frame->clock, frame->applied) : -1;
    s->us[FRAME_APPLY_SCREEN][n] = frame->screen > 0 ? split_micros(frame->applied, frame->screen) : -1;
    s->us[FRAME_CLOCK_LATCH][n] = frame->latched > 0 ? split_micros(frame->clock, frame->latched) : -1;
    s->last_presented = frame->presented;
    return done;
}
