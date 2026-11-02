/*
 * The counts behind two of x-input-relay's log lines (x-input-relay.c), apart from Xlib so that a
 * host program tests them (app/src/test/cpp/relay_counts_test.c). Each line comes at the end of a
 * window that starts with the first event it counts, so a player who pushes at an edge for a
 * minute gives a few lines, not one line for each move:
 *   - the moves by a distance that the pointer area's edges changed (move_by()), by edge: one line
 *     at most each EDGE_WINDOW_NS, with the count and the last such move;
 *   - the pointer at a place other than where the relay last put it (the game or Wine moved it:
 *     PD2 puts it on the character when the controller's stick walks): one line at most each
 *     MOVED_WINDOW_NS, with the first such move and how many more came;
 *   - the relay's own time for each move of the pointer from the app, from the read of its event
 *     to XFlush (the X server's queue has it then; the X server's own time is not in it): one line
 *     at most each EDGE_WINDOW_NS while moves come, with the count, the median, the 95th
 *     percentile (of the first MOVE_SAMPLES) and the longest.
 * Times are CLOCK_MONOTONIC nanoseconds; 0 is no time.
 */
#pragma once

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define EDGE_WINDOW_NS 5000000000LL
#define MOVED_WINDOW_NS 10000000000LL

/* The pointer area's edges, as a move by a distance meets them; their bits in a stop's [edges]. */
enum { EDGE_LEFT, EDGE_RIGHT, EDGE_TOP, EDGE_BOTTOM, EDGES };

struct edge_stops {
    int64_t since_ns; /* the first stop of the open window; 0: no window is open */
    unsigned count[EDGES];
    int last_dx, last_dy, last_x, last_y; /* the last stop: the move and where it started */
};

/* The edges that change a move by [dx], [dy] from [x], [y] in the area from [left], [top] to
 * [right] - 1, [bottom] - 1: a bit (1 << EDGE_*) for each edge the move would pass or start
 * beyond. */
static inline int edges_met(int x, int y, int dx, int dy, int left, int top, int right, int bottom) {
    int64_t to_x = (int64_t)x + dx, to_y = (int64_t)y + dy;
    int edges = 0;
    if (to_x < left) edges |= 1 << EDGE_LEFT;
    if (to_x >= right) edges |= 1 << EDGE_RIGHT;
    if (to_y < top) edges |= 1 << EDGE_TOP;
    if (to_y >= bottom) edges |= 1 << EDGE_BOTTOM;
    return edges;
}

/* A move by [dx], [dy] from [x], [y] that the area's [edges] changed, at [now]. */
static inline void edge_stop(struct edge_stops *s, int64_t now, int edges, int dx, int dy, int x, int y) {
    if (edges == 0) return;
    if (s->since_ns == 0) s->since_ns = now;
    for (int e = 0; e < EDGES; e++) {
        if (edges & (1 << e)) s->count[e]++;
    }
    s->last_dx = dx;
    s->last_dy = dy;
    s->last_x = x;
    s->last_y = y;
}

/* When the open window ends (its line is due); 0 while none is open. */
static inline int64_t edge_due(const struct edge_stops *s) {
    return s->since_ns == 0 ? 0 : s->since_ns + EDGE_WINDOW_NS;
}

/* The line for the window when it is due at [now] (or at once with [force], at the end), into
 * [out] ([cap] bytes), for the area [width] x [height] at [left], [top]; the window closes. 1 when
 * there is a line, else 0. */
static inline int edge_line(struct edge_stops *s, int64_t now, int force, int left, int top, int width, int height, char *out,
                            size_t cap) {
    static const char *const names[EDGES] = {"left", "right", "top", "bottom"};
    if (s->since_ns == 0 || (!force && now < edge_due(s))) return 0;
    size_t used = (size_t)snprintf(out, cap, "x-input-relay: moves stopped at the pointer area's");
    int first = 1;
    for (int e = 0; e < EDGES && used < cap; e++) {
        if (s->count[e] == 0) continue;
        used += (size_t)snprintf(out + used, cap - used, "%s %s edge: %u", first ? "" : ",", names[e], s->count[e]);
        first = 0;
    }
    if (used < cap) {
        snprintf(out + used, cap - used, " in %lld s (the last by %d,%d from %d,%d; area %dx%d at %d,%d)",
                 (long long)(EDGE_WINDOW_NS / 1000000000LL), s->last_dx, s->last_dy, s->last_x, s->last_y, width, height, left,
                 top);
    }
    struct edge_stops none = {0, {0}, 0, 0, 0, 0};
    *s = none;
    return 1;
}

struct foreign_moves {
    int known;          /* the relay knows where it last put the pointer: [at_x], [at_y] */
    int at_x, at_y;
    int64_t since_ns;   /* the first such move of the open window; 0: no window is open */
    unsigned more;      /* the moves after the first in the window */
    int from_x, from_y, to_x, to_y; /* the first move */
};

/* The relay put the pointer at [x], [y] (an absolute move, or a move by a distance it placed). */
static inline void pointer_put(struct foreign_moves *m, int x, int y) {
    m->known = 1;
    m->at_x = x;
    m->at_y = y;
}

/* The relay moved the pointer by a distance with no area: the X server places it, so the place is
 * not known until the relay puts it somewhere again. */
static inline void pointer_unknown(struct foreign_moves *m) {
    m->known = 0;
}

/* XQueryPointer found the pointer at [x], [y] at [now]: another place than where the relay put it
 * is a move by the game or Wine, which counts. The place found is the one known from now on. */
static inline void pointer_seen(struct foreign_moves *m, int64_t now, int x, int y) {
    if (m->known && (x != m->at_x || y != m->at_y)) {
        if (m->since_ns == 0) {
            m->since_ns = now;
            m->more = 0;
            m->from_x = m->at_x;
            m->from_y = m->at_y;
            m->to_x = x;
            m->to_y = y;
        } else {
            m->more++;
        }
    }
    pointer_put(m, x, y);
}

/* When the open window ends (its line is due); 0 while none is open. */
static inline int64_t moved_due(const struct foreign_moves *m) {
    return m->since_ns == 0 ? 0 : m->since_ns + MOVED_WINDOW_NS;
}

/* The line for the window when it is due at [now] (or at once with [force], at the end), into
 * [out] ([cap] bytes); the window closes. 1 when there is a line, else 0. */
static inline int moved_line(struct foreign_moves *m, int64_t now, int force, char *out, size_t cap) {
    if (m->since_ns == 0 || (!force && now < moved_due(m))) return 0;
    if (m->more > 0) {
        snprintf(out, cap, "x-input-relay: the pointer moved without the relay: from %d,%d to %d,%d (the game or Wine; %u more in %lld s)",
                 m->from_x, m->from_y, m->to_x, m->to_y, m->more, (long long)(MOVED_WINDOW_NS / 1000000000LL));
    } else {
        snprintf(out, cap, "x-input-relay: the pointer moved without the relay: from %d,%d to %d,%d (the game or Wine)", m->from_x,
                 m->from_y, m->to_x, m->to_y);
    }
    m->since_ns = 0;
    m->more = 0;
    return 1;
}

/* The earliest of two due times ([edge_due], [moved_due], [move_times_due]), 0 for none. */
static inline int64_t first_due(int64_t a, int64_t b) {
    if (a == 0) return b;
    if (b == 0) return a;
    return a < b ? a : b;
}

/* The moves of a window whose times the percentiles take: at 240 moves a second, 5 s have 1200. */
#define MOVE_SAMPLES 2048

struct move_times {
    int64_t since_ns; /* the first move of the open window; 0: no window is open */
    unsigned count, kept;
    uint32_t max_us;
    uint32_t us[MOVE_SAMPLES]; /* the first [kept] moves' times, in microseconds */
};

/* A move whose event the relay read at [now] and had in the X server's queue [took_ns] later. */
static inline void move_time(struct move_times *m, int64_t now, int64_t took_ns) {
    uint32_t us = took_ns <= 0 ? 0 : took_ns / 1000 > UINT32_MAX ? UINT32_MAX : (uint32_t)(took_ns / 1000);
    if (m->since_ns == 0) {
        m->since_ns = now;
        m->count = m->kept = 0;
        m->max_us = 0;
    }
    m->count++;
    if (m->kept < MOVE_SAMPLES) m->us[m->kept++] = us;
    if (us > m->max_us) m->max_us = us;
}

/* When the open window ends (its line is due); 0 while none is open. */
static inline int64_t move_times_due(const struct move_times *m) {
    return m->since_ns == 0 ? 0 : m->since_ns + EDGE_WINDOW_NS;
}

static inline int compare_us(const void *a, const void *b) {
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return x < y ? -1 : x > y;
}

/* The time at [fraction] of the [n] sorted times [us], by the nearest rank. */
static inline uint32_t move_rank(const uint32_t *us, unsigned n, double fraction) {
    double rank = fraction * n;
    unsigned at = (unsigned)rank;
    if ((double)at < rank) at++;
    return us[at == 0 ? 0 : at > n ? n - 1 : at - 1];
}

/* The line for the window when it is due at [now] (or at once with [force], at the end), into
 * [out] ([cap] bytes); the window closes. 1 when there is a line, else 0. */
static inline int move_times_line(struct move_times *m, int64_t now, int force, char *out, size_t cap) {
    if (m->since_ns == 0 || (!force && now < move_times_due(m))) return 0;
    qsort(m->us, m->kept, sizeof m->us[0], compare_us);
    snprintf(out, cap,
             "x-input-relay: pointer moves in %lld s: %u, from the read to the X server's queue: median %.1f ms, p95 %.1f ms, max %.1f ms",
             (long long)(EDGE_WINDOW_NS / 1000000000LL), m->count, move_rank(m->us, m->kept, 0.5) / 1000.0,
             move_rank(m->us, m->kept, 0.95) / 1000.0, m->max_us / 1000.0);
    m->since_ns = 0;
    return 1;
}
