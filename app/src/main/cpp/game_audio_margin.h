/*
 * The game's sound (game_audio.c): when the ring starts playing after the writer's stream starts,
 * and how big a cushion the reader asks Wine to keep queued (pd2_audio_ring.h). Wine's
 * DirectSound keeps only about 50 ms ahead, and its mixing (x86 code under Box64) stalls while
 * the game loads: a new stream of the game (the game's start, its menus loading next) starts with
 * a large cushion; after a callback finds the ring short, the cushion grows by a step (at most one
 * step per hold time, so one stall raises it once), up to a maximum; after a calm time it shrinks
 * back a little every second, so steady play returns to the lowest delay.
 */
#ifndef GAME_AUDIO_MARGIN_H
#define GAME_AUDIO_MARGIN_H

#include <stdint.h>

#include "pd2_audio_ring.h"

#define GA_MARGIN_START (PD2_AUDIO_RATE * 160 / 1000)  /* 160 ms */
#define GA_MARGIN_STEP (PD2_AUDIO_RATE * 40 / 1000)    /* 40 ms */
#define GA_MARGIN_MAX (PD2_AUDIO_RATE * 200 / 1000)    /* 200 ms */
#define GA_MARGIN_SHRINK (PD2_AUDIO_RATE * 2 / 1000)   /* 2 ms a second */
#define GA_MARGIN_HOLD_NS 200000000LL                  /* 0.2 s */
#define GA_MARGIN_CALM_NS 30000000000LL                /* 30 s */

struct ga_margin {
    uint32_t frames;
    int64_t raised;
    int64_t calm;
    int64_t shrunk;
};

static inline void ga_margin_init(struct ga_margin *m)
{
    m->frames = 0;
    m->raised = INT64_MIN / 2;
    m->calm = INT64_MIN / 2;
    m->shrunk = INT64_MIN / 2;
}

/* A new stream of the game starts playing at now (ns): at least the start cushion, calm from now. */
static inline uint32_t ga_margin_start(struct ga_margin *m, int64_t now)
{
    if (m->frames < GA_MARGIN_START)
        m->frames = GA_MARGIN_START;
    m->calm = now;
    m->shrunk = now;
    return m->frames;
}

/* After a callback at now (ns) that found the ring short ([dry]) or not; returns the cushion. */
static inline uint32_t ga_margin_update(struct ga_margin *m, int dry, int64_t now)
{
    if (dry) {
        if (now - m->raised >= GA_MARGIN_HOLD_NS && m->frames < GA_MARGIN_MAX) {
            m->frames = m->frames + GA_MARGIN_STEP > GA_MARGIN_MAX ? GA_MARGIN_MAX : m->frames + GA_MARGIN_STEP;
            m->raised = now;
        }
        m->calm = now;
        m->shrunk = now;
    } else if (m->frames && now - m->calm >= GA_MARGIN_CALM_NS && now - m->shrunk >= 1000000000LL) {
        m->frames = m->frames > GA_MARGIN_SHRINK ? m->frames - GA_MARGIN_SHRINK : 0;
        m->shrunk = now;
    }
    return m->frames;
}

/*
 * Whether the reader may start taking frames after the writer's stream started: once the ring
 * holds two callbacks' input ([need] each), so the start is not short.
 */
static inline int ga_margin_primed(uint32_t ready, uint32_t need)
{
    return ready >= 2 * need;
}

/* What one callback does with the ring (ga_margin_take). */
struct ga_take {
    int primed;     /* the writer's stream plays: the reader takes frames */
    int started;    /* it began to play in this callback: the start cushion applies */
    uint32_t take;  /* frames the reader takes */
    int dry;        /* fewer than needed while it plays: a short read */
};

/*
 * One callback's decision: [primed] from the callback before, whether the writer's stream
 * [running], the frames [ready] in the ring and the frames the callback [need]s. A stopped stream
 * is not primed and takes nothing; a new one waits until it is primed; a short read counts only
 * while it plays.
 */
static inline struct ga_take ga_margin_take(int primed, int running, uint32_t ready, uint32_t need)
{
    struct ga_take t = {0, 0, 0, 0};
    if (!running)
        return t;
    t.primed = primed || ga_margin_primed(ready, need);
    t.started = t.primed && !primed;
    if (t.primed) {
        t.take = ready < need ? ready : need;
        t.dry = t.take < need;
    }
    return t;
}

#endif
