/*
 * The game's sound from its 22.05 kHz to the phone's own output rate (game_audio.c): a polyphase
 * windowed-sinc (Kaiser) resampler for interleaved float stereo. The phone's rate keeps the
 * output on Android's fast mixer path; a track at 22.05 kHz goes through the slower normal mixer.
 */
#ifndef GAME_AUDIO_RESAMPLER_H
#define GAME_AUDIO_RESAMPLER_H

#include <stdint.h>

/* Taps of each phase: a pass band flat to about 8 kHz and 70 dB less above 11 kHz at 22.05 kHz in. */
#define GA_TAPS 32

struct ga_resampler {
    uint32_t in_rate;
    uint32_t out_rate;
    uint32_t up;    /* L: out_rate / gcd */
    uint32_t down;  /* M: in_rate / gcd */
    uint32_t phase; /* 0 .. up - 1 */
    uint32_t pending; /* input frames to take in before the next output frame */
    uint32_t newest;  /* index of the newest input in history (0 .. GA_TAPS - 1) */
    float *coeffs;    /* up phases of GA_TAPS, the newest input first */
    float history[2][2 * GA_TAPS];
};

/* 0, or -1 when a rate is 0 or the table cannot be made. */
int ga_resampler_init(struct ga_resampler *r, uint32_t in_rate, uint32_t out_rate);
void ga_resampler_free(struct ga_resampler *r);

/* Input frames the next ga_resampler_process of out_frames output frames takes. */
uint32_t ga_resampler_needed(const struct ga_resampler *r, uint32_t out_frames);

/* out_frames output frames into out from exactly ga_resampler_needed(r, out_frames) input frames. */
void ga_resampler_process(struct ga_resampler *r, const float *in, float *out, uint32_t out_frames);

/* The filter's delay in input frames (half its length). */
static inline uint32_t ga_resampler_delay(void) { return GA_TAPS / 2; }

#endif
