#include "game_audio_resampler.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

/* Kaiser window parameter for about 70 dB of stop band attenuation. */
#define BETA 6.76

static uint32_t gcd(uint32_t a, uint32_t b)
{
    while (b) {
        uint32_t t = a % b;
        a = b;
        b = t;
    }
    return a;
}

/* The zeroth order modified Bessel function of the first kind, by its series. */
static double bessel_i0(double x)
{
    double sum = 1.0, term = 1.0, q = x * x / 4.0;
    for (int k = 1; k < 64; k++) {
        term *= q / ((double)k * k);
        sum += term;
        if (term < sum * 1e-16)
            break;
    }
    return sum;
}

int ga_resampler_init(struct ga_resampler *r, uint32_t in_rate, uint32_t out_rate)
{
    memset(r, 0, sizeof(*r));
    if (!in_rate || !out_rate)
        return -1;
    uint32_t g = gcd(in_rate, out_rate);
    r->in_rate = in_rate;
    r->out_rate = out_rate;
    r->up = out_rate / g;
    r->down = in_rate / g;
    r->pending = 1;
    r->coeffs = malloc((size_t)r->up * GA_TAPS * sizeof(float));
    if (!r->coeffs)
        return -1;

    /* The prototype low pass runs at up * in_rate and has up * GA_TAPS taps. Its cut off (the
     * half-amplitude point) sits half a transition band below the lower Nyquist frequency. */
    double fs = (double)r->up * in_rate;
    double nyquist = 0.5 * (in_rate < out_rate ? in_rate : out_rate);
    double transition = (70.0 - 8.0) / (14.36 * GA_TAPS) * in_rate;
    double cutoff = (nyquist - transition / 2) / fs;
    uint32_t n = r->up * GA_TAPS;
    double centre = (n - 1) / 2.0;
    double i0beta = bessel_i0(BETA);
    for (uint32_t p = 0; p < r->up; p++) {
        double sum = 0;
        for (uint32_t k = 0; k < GA_TAPS; k++) {
            /* Phase p's tap k weighs the input k frames before the newest. */
            uint32_t i = p + k * r->up;
            double t = i - centre;
            double sinc = t == 0 ? 2 * cutoff : sin(2 * M_PI * cutoff * t) / (M_PI * t);
            double w = 2.0 * i / (n - 1) - 1.0;
            double window = bessel_i0(BETA * sqrt(fmax(0.0, 1.0 - w * w))) / i0beta;
            double c = sinc * window;
            r->coeffs[p * GA_TAPS + k] = (float)c;
            sum += c;
        }
        /* Each phase passes a constant signal unchanged. */
        for (uint32_t k = 0; k < GA_TAPS; k++)
            r->coeffs[p * GA_TAPS + k] = (float)(r->coeffs[p * GA_TAPS + k] / sum);
    }
    return 0;
}

void ga_resampler_free(struct ga_resampler *r)
{
    free(r->coeffs);
    r->coeffs = NULL;
}

uint32_t ga_resampler_needed(const struct ga_resampler *r, uint32_t out_frames)
{
    if (!out_frames)
        return 0;
    return r->pending + (uint32_t)(((uint64_t)r->phase + (uint64_t)(out_frames - 1) * r->down) / r->up);
}

static inline void push(struct ga_resampler *r, const float *frame)
{
    r->newest = r->newest == 0 ? GA_TAPS - 1 : r->newest - 1;
    for (int c = 0; c < 2; c++) {
        r->history[c][r->newest] = frame[c];
        r->history[c][r->newest + GA_TAPS] = frame[c];
    }
}

void ga_resampler_process(struct ga_resampler *r, const float *in, float *out, uint32_t out_frames)
{
    for (uint32_t j = 0; j < out_frames; j++) {
        for (uint32_t i = 0; i < r->pending; i++) {
            push(r, in);
            in += 2;
        }
        const float *c = r->coeffs + r->phase * GA_TAPS;
        const float *left = r->history[0] + r->newest;
        const float *right = r->history[1] + r->newest;
        float l = 0, rr = 0;
        for (uint32_t k = 0; k < GA_TAPS; k++) {
            l += c[k] * left[k];
            rr += c[k] * right[k];
        }
        out[2 * j] = l;
        out[2 * j + 1] = rr;
        uint32_t next = r->phase + r->down;
        r->pending = next / r->up;
        r->phase = next % r->up;
    }
}
