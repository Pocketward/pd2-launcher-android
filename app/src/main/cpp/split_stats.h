// The statistics that the pointer split (pointer_split.c) and the frame split (frame_split.c)
// share: times in whole microseconds, and the median and 95th percentile by the nearest rank.
#pragma once
#include <stdint.h>
#include <stdlib.h>

// From [from] to [to] in whole microseconds, at least 0.
static inline int32_t split_micros(int64_t from, int64_t to) {
    int64_t d = to - from;
    if (d <= 0) return 0;
    d = (d + 500) / 1000;
    return d > INT32_MAX ? INT32_MAX : (int32_t)d;
}

static inline int split_ascending(const void *a, const void *b) {
    int32_t x = *(const int32_t *)a, y = *(const int32_t *)b;
    return (x > y) - (x < y);
}

// The value at [percent] of the [n] values of [sorted], by the nearest rank.
static inline int32_t split_rank(const int32_t *sorted, int n, int percent) {
    int i = (n * percent + 99) / 100 - 1;
    return sorted[i < 0 ? 0 : i >= n ? n - 1 : i];
}

// Sorts the [n] values of [values] and gives their median and 95th percentile; a value below 0 is
// not known and not in the ranks; -1 for both when none is known.
static inline void split_ranks(int32_t *values, int n, int32_t *median, int32_t *p95) {
    qsort(values, (size_t)n, sizeof(int32_t), split_ascending);
    int unknown = 0;
    while (unknown < n && values[unknown] < 0) unknown++;
    int known = n - unknown;
    *median = known > 0 ? split_rank(values + unknown, known, 50) : -1;
    *p95 = known > 0 ? split_rank(values + unknown, known, 95) : -1;
}
