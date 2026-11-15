// Detailed logging's split of the pointer's delay after the app's write into x-input-relay's
// socket (pointer_split.h).
#include "pointer_split.h"
#include "split_stats.h"

#include <stdlib.h>
#include <string.h>

int split_read(const uint32_t *page, uint32_t serial, struct split_frame *frame) {
    if (page == NULL || serial == 0) return 0;
    const uint8_t *slot = (const uint8_t *)page + SPLIT_RING_BYTE + (serial % SPLIT_SLOTS) * SPLIT_SLOT_BYTES;
    const uint32_t *slot_serial = (const uint32_t *)slot;
    const uint64_t *times = (const uint64_t *)(slot + 8);
    if (__atomic_load_n(slot_serial, __ATOMIC_ACQUIRE) != serial) return 0;
    uint64_t t[4];
    for (int i = 0; i < 4; i++) t[i] = __atomic_load_n(&times[i], __ATOMIC_RELAXED);
    // The same serial after the copy: Mesa did not write the slot meanwhile.
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    if (__atomic_load_n(slot_serial, __ATOMIC_RELAXED) != serial) return 0;
    memset(frame, 0, sizeof(*frame));
    frame->acquired = (int64_t)t[0];
    frame->presented = (int64_t)t[1];
    frame->gpu_done = (int64_t)t[2];
    frame->clock = (int64_t)t[3];
    return 1;
}

void split_reset(struct pointer_split *s) {
    s->first = s->count = 0;
    s->since = s->last = 0;
    s->samples = s->frames = s->not_counted = 0;
}

void split_move(struct pointer_split *s, int64_t written) {
    if (s->count == SPLIT_MOVES) {
        s->first = (s->first + 1) % SPLIT_MOVES;
        s->count--;
        s->not_counted++;
    }
    s->moves[(s->first + s->count) % SPLIT_MOVES] = written;
    s->count++;
}

// The window's summary into [out]; the next window starts empty.
static void summarize(struct pointer_split *s, struct split_summary *out) {
    memset(out, 0, sizeof(*out));
    out->span_ns = s->last - s->since;
    out->moves = s->samples;
    out->frames = s->frames;
    out->not_counted = s->not_counted;
    // A time not known is -1 (clock -> latch without a latch time): not in the ranks.
    for (int k = 0; k < SPLIT_PARTS && s->samples > 0; k++) split_ranks(s->us[k], s->samples, &out->median_us[k], &out->p95_us[k]);
    s->since = s->last = 0;
    s->samples = s->frames = s->not_counted = 0;
}

int split_shown(struct pointer_split *s, const struct split_frame *frame, struct split_summary *ended) {
    int done = 0;
    if (s->since != 0 && (frame->applied - s->since >= SPLIT_WINDOW_NS || s->samples == SPLIT_SAMPLES)) {
        summarize(s, ended);
        done = 1;
    }
    int took = 0, counted = 0;
    while (s->count > 0 && s->moves[s->first] <= frame->acquired) {
        int64_t written = s->moves[s->first];
        s->first = (s->first + 1) % SPLIT_MOVES;
        s->count--;
        took = 1;
        if (frame->screen == 0 || frame->acquired - written > SPLIT_OLDEST_NS || s->samples == SPLIT_SAMPLES) {
            s->not_counted++;
            continue;
        }
        int n = s->samples++;
        s->us[SPLIT_WRITE_ACQUIRE][n] = split_micros(written, frame->acquired);
        s->us[SPLIT_ACQUIRE_PRESENT][n] = split_micros(frame->acquired, frame->presented);
        s->us[SPLIT_PRESENT_CLOCK][n] = split_micros(frame->presented, frame->clock);
        s->us[SPLIT_PRESENT_GPU][n] = split_micros(frame->presented, frame->gpu_done);
        s->us[SPLIT_CLOCK_APPLY][n] = split_micros(frame->clock, frame->applied);
        s->us[SPLIT_APPLY_SCREEN][n] = split_micros(frame->applied, frame->screen);
        s->us[SPLIT_CLOCK_LATCH][n] = frame->latched > 0 ? split_micros(frame->clock, frame->latched) : -1;
        s->us[SPLIT_WRITE_SCREEN][n] = split_micros(written, frame->screen);
        counted = 1;
    }
    if (took) {
        if (s->since == 0) s->since = frame->applied;
        s->last = frame->applied;
    }
    if (counted) s->frames++;
    return done;
}
