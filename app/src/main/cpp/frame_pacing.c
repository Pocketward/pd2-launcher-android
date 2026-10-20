// The Game view's rule for a reduced frame rate (frame_pacing.h): pure functions of the display
// frames' times, used by frame_copy.c and present_server.cpp.
#include "frame_pacing.h"

// Display frames in a row with a gap that does not fit the measured period before it is measured
// again: the display changed its rate (a missed display frame's double gap alone does not count).
#define PERIOD_OUTLIERS 8

void take_display(struct take_schedule *s, int64_t frame_ns) {
    int64_t gap = s->frame_ns != 0 ? frame_ns - s->frame_ns : 0;
    s->frame_ns = frame_ns;
    if (gap <= 0) return;
    if (s->period_ns == 0) {
        s->period_ns = gap;
    } else if (gap * 4 >= s->period_ns * 3 && gap * 4 <= s->period_ns * 5) {
        // Within a quarter of the period: it moves by an eighth of the difference.
        s->period_ns += (gap - s->period_ns) / 8;
        s->outliers = 0;
    } else if (++s->outliers >= PERIOD_OUTLIERS) {
        s->period_ns = gap;
        s->outliers = 0;
    }
}

// The display frames per take where the interval is a whole number of them (within 2 %), else 0.
static int64_t whole_frames(const struct take_schedule *s) {
    if (s->period_ns <= 0) return 0;
    int64_t n = (s->interval_ns + s->period_ns / 2) / s->period_ns;
    if (n < 1) return 0;
    int64_t off = n * s->period_ns - s->interval_ns;
    if (off < 0) off = -off;
    return off * 50 <= s->interval_ns ? n : 0;
}

int take_due(struct take_schedule *s, int64_t frame_ns, int pending) {
    if (s->interval_ns <= 0 || pending) return 1;
    int64_t n = whole_frames(s);
    int take;
    if (s->last_ns == 0) {
        take = 1;
    } else if (n > 0) {
        take = frame_ns - s->last_ns >= n * s->period_ns - s->period_ns / 2;
    } else {
        take = frame_ns >= s->next_ns - TAKE_EARLY_NS;
    }
    if (!take) return 0;
    if (s->next_ns == 0 || n > 0 || frame_ns - s->next_ns > s->interval_ns) {
        s->next_ns = frame_ns + s->interval_ns;
    } else {
        s->next_ns += s->interval_ns;
    }
    s->last_ns = frame_ns;
    return 1;
}
