/*
 * The game's sound (game_audio.c) while its output stream is gone (another output device,
 * Bluetooth turned off, for example): how long the restarter waits before it opens the stream
 * again, and the ring drained at the game's rate meanwhile, so Wine's DirectSound sees its play
 * position move as if the frames played (the data is dropped).
 */
#ifndef GAME_AUDIO_RESTART_H
#define GAME_AUDIO_RESTART_H

#include <stdint.h>

#include "pd2_audio_ring.h"

/* A stream that stays up this long ends a run of failures. */
#define GA_STABLE_NS 10000000000LL /* 10 s */

/* The wait in seconds before the next try after [failures] tries in a row that gave no stream that
 * stayed up: none at first, then 1, 2 and 4 s, then 10 s. */
static inline int ga_restart_wait(int failures)
{
    if (failures < 1)
        return 0;
    return failures < 4 ? 1 << (failures - 1) : 10;
}

/* The failures in a row once the stream that opened [up_ns] ago went: none when it stayed up
 * GA_STABLE_NS or longer, else one more. */
static inline int ga_restart_failures(int failures, int64_t up_ns)
{
    return up_ns >= GA_STABLE_NS ? 0 : failures + 1;
}

/* The frames due at [rate] for [elapsed_ns] (at most one second at a time), the remainder kept in
 * *carry (in ns times frames a second) for the next call. */
static inline uint32_t ga_drain_due(uint64_t *carry, int64_t elapsed_ns, uint32_t rate)
{
    if (elapsed_ns <= 0)
        return 0;
    if (elapsed_ns > 1000000000LL)
        elapsed_ns = 1000000000LL;
    uint64_t total = *carry + (uint64_t)elapsed_ns * rate;
    *carry = total % 1000000000ULL;
    return (uint32_t)(total / 1000000000ULL);
}

/* Takes at most [due] frames from the ring and drops them, through [scratch] ([scratch_frames]
 * frames); the frames taken. Only while nothing else reads the ring. */
static inline uint32_t ga_drain(struct pd2_audio_ring *ring, float *scratch, uint32_t scratch_frames, uint32_t due)
{
    uint32_t ready = pd2_audio_readable(ring);
    uint32_t take = ready < due ? ready : due;
    for (uint32_t left = take; left > 0;) {
        uint32_t n = left < scratch_frames ? left : scratch_frames;
        pd2_audio_read(ring, scratch, n);
        left -= n;
    }
    return take;
}

#endif
