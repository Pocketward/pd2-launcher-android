/*
 * The game's sound between Wine and the Android app: a ring of float stereo frames at
 * 22.05 kHz in a file both map (MAP_SHARED). The app creates the file and plays it; the game's
 * Wine writes it through ALSA (winealsa.drv, then the ALSA plugin pcm_pd2.c). One writer and
 * one reader, no locks:
 *
 *   write_pos   frames written since the ring was made (the writer's; it only grows);
 *   start_pos   the first frame of the writer's current stream (a stop or a new stream moves it
 *               to write_pos: what is left before it is never played);
 *   running     the writer's stream is started: the reader plays only then;
 *   read_pos    frames the reader has taken (the reader's; it only grows, and jumps to start_pos
 *               when it is behind it);
 *   cushion     frames the reader wants queued beyond what Wine keeps ahead (the reader's).
 *
 * The writer's ALSA hardware position is read_pos - start_pos + cushion (pd2_audio_hw), so Wine's
 * DirectSound sees the frames leave as the app plays them and keeps its own lead (three times
 * winealsa's latency) plus the cushion queued. The reader raises the cushion when the ring runs
 * dry (Wine's mixing, x86 code under Box64, stalls while the game loads) and lowers it again
 * when it stays calm; a lower cushion only holds the position still until the reader catches up.
 * The ring never holds more than the writer's ALSA buffer plus the cushion (at most
 * PD2_AUDIO_FRAMES). The positions live in their own cache lines. Used by C on both sides
 * (glibc in Wine, bionic in the app), on aarch64.
 */
#ifndef PD2_AUDIO_RING_H
#define PD2_AUDIO_RING_H

#include <stdint.h>
#include <string.h>

#define PD2_AUDIO_MAGIC 0x41324450u /* "PD2A" */
#define PD2_AUDIO_VERSION 2u
#define PD2_AUDIO_RATE 22050u
#define PD2_AUDIO_CHANNELS 2u
/* The ring's capacity in frames, a power of two: 743 ms. Half of it is the most the writer's ALSA
 * buffer may hold (pcm_pd2.c), the other half the most the reader's cushion may be. */
#define PD2_AUDIO_FRAMES 16384u
#define PD2_AUDIO_HEADER 4096u
#define PD2_AUDIO_FILE_SIZE (PD2_AUDIO_HEADER + PD2_AUDIO_FRAMES * PD2_AUDIO_CHANNELS * sizeof(float))

struct pd2_audio_ring {
    /* Set once by the app before any writer maps the file. */
    uint32_t magic;
    uint32_t version;
    uint32_t rate;
    uint32_t channels;
    uint32_t frames;
    uint32_t reserved0[11];
    /* The writer's. */
    uint64_t write_pos __attribute__((aligned(64)));
    uint64_t start_pos __attribute__((aligned(8)));
    uint32_t running;
    int32_t owner; /* the writing process's pid, 0 when none */
    /* The reader's. */
    uint64_t read_pos __attribute__((aligned(64)));
    uint64_t underruns __attribute__((aligned(8))); /* times the ring ran dry while the writer's stream ran */
    uint32_t cushion;
};

_Static_assert(sizeof(struct pd2_audio_ring) <= PD2_AUDIO_HEADER, "the ring's header fits its page");

static inline float *pd2_audio_data(struct pd2_audio_ring *ring)
{
    return (float *)((char *)ring + PD2_AUDIO_HEADER);
}

/* The app: a new ring in the mapped file. */
static inline void pd2_audio_init(struct pd2_audio_ring *ring)
{
    memset(ring, 0, PD2_AUDIO_HEADER);
    ring->magic = PD2_AUDIO_MAGIC;
    ring->version = PD2_AUDIO_VERSION;
    ring->rate = PD2_AUDIO_RATE;
    ring->channels = PD2_AUDIO_CHANNELS;
    ring->frames = PD2_AUDIO_FRAMES;
}

/* Whether a mapped file of size bytes holds a ring of this layout. */
static inline int pd2_audio_valid(const struct pd2_audio_ring *ring, uint64_t size)
{
    return size >= PD2_AUDIO_FILE_SIZE && ring->magic == PD2_AUDIO_MAGIC && ring->version == PD2_AUDIO_VERSION &&
        ring->rate == PD2_AUDIO_RATE && ring->channels == PD2_AUDIO_CHANNELS && ring->frames == PD2_AUDIO_FRAMES;
}

/* --- The writer (pcm_pd2.c) --- */

/* A new stream: what is in the ring is dropped, nothing plays until pd2_audio_start. */
static inline void pd2_audio_prepare(struct pd2_audio_ring *ring)
{
    __atomic_store_n(&ring->running, 0, __ATOMIC_RELEASE);
    __atomic_store_n(&ring->start_pos, __atomic_load_n(&ring->write_pos, __ATOMIC_RELAXED), __ATOMIC_RELEASE);
}

static inline void pd2_audio_start(struct pd2_audio_ring *ring)
{
    __atomic_store_n(&ring->running, 1, __ATOMIC_RELEASE);
}

/* The stream stops; what it left in the ring is dropped. */
static inline void pd2_audio_stop(struct pd2_audio_ring *ring)
{
    pd2_audio_prepare(ring);
}

/* Frames of the current stream the reader has taken. */
static inline uint64_t pd2_audio_played(struct pd2_audio_ring *ring)
{
    uint64_t start = __atomic_load_n(&ring->start_pos, __ATOMIC_RELAXED);
    uint64_t read = __atomic_load_n(&ring->read_pos, __ATOMIC_ACQUIRE);
    return read > start ? read - start : 0;
}

/* Frames of the current stream written so far. */
static inline uint64_t pd2_audio_written(struct pd2_audio_ring *ring)
{
    return __atomic_load_n(&ring->write_pos, __ATOMIC_RELAXED) - __atomic_load_n(&ring->start_pos, __ATOMIC_RELAXED);
}

/*
 * The writer's ALSA hardware position in its current stream: what the reader took plus the
 * cushion, never past what was written and never behind the position given before (*last, 0 at
 * the stream's prepare).
 */
static inline uint64_t pd2_audio_hw(struct pd2_audio_ring *ring, uint64_t *last)
{
    /* The reader's value, but never more than half the ring (the other half is the ALSA buffer's). */
    uint32_t cushion = __atomic_load_n(&ring->cushion, __ATOMIC_ACQUIRE);
    if (cushion > PD2_AUDIO_FRAMES / 2)
        cushion = PD2_AUDIO_FRAMES / 2;
    uint64_t hw = pd2_audio_played(ring) + cushion;
    uint64_t written = pd2_audio_written(ring);
    if (hw > written)
        hw = written;
    if (hw < *last)
        hw = *last;
    *last = hw;
    return hw;
}

/* Appends count interleaved stereo frames; the caller keeps the ring from overflowing (its ALSA
 * buffer plus the cushion is at most PD2_AUDIO_FRAMES). */
static inline void pd2_audio_write(struct pd2_audio_ring *ring, const float *frames, uint32_t count)
{
    float *data = pd2_audio_data(ring);
    uint64_t pos = __atomic_load_n(&ring->write_pos, __ATOMIC_RELAXED);
    uint32_t at = (uint32_t)(pos & (PD2_AUDIO_FRAMES - 1));
    uint32_t first = count < PD2_AUDIO_FRAMES - at ? count : PD2_AUDIO_FRAMES - at;
    memcpy(data + at * PD2_AUDIO_CHANNELS, frames, first * PD2_AUDIO_CHANNELS * sizeof(float));
    memcpy(data, frames + first * PD2_AUDIO_CHANNELS, (count - first) * PD2_AUDIO_CHANNELS * sizeof(float));
    __atomic_store_n(&ring->write_pos, pos + count, __ATOMIC_RELEASE);
}

/* --- The reader (the app) --- */

/* Frames the reader may take now: none while the writer's stream does not run. */
static inline uint32_t pd2_audio_readable(struct pd2_audio_ring *ring)
{
    uint64_t start = __atomic_load_n(&ring->start_pos, __ATOMIC_ACQUIRE);
    uint64_t read = __atomic_load_n(&ring->read_pos, __ATOMIC_RELAXED);
    if (read < start) {
        read = start;
        __atomic_store_n(&ring->read_pos, read, __ATOMIC_RELEASE);
    }
    if (!__atomic_load_n(&ring->running, __ATOMIC_ACQUIRE))
        return 0;
    uint64_t write = __atomic_load_n(&ring->write_pos, __ATOMIC_ACQUIRE);
    uint64_t ready = write > read ? write - read : 0;
    return ready > PD2_AUDIO_FRAMES ? PD2_AUDIO_FRAMES : (uint32_t)ready;
}

/* The reader's cushion, in frames (at most PD2_AUDIO_FRAMES / 2). */
static inline void pd2_audio_set_cushion(struct pd2_audio_ring *ring, uint32_t frames)
{
    __atomic_store_n(&ring->cushion, frames, __ATOMIC_RELEASE);
}

/* Takes count frames (at most pd2_audio_readable's) into out, interleaved. */
static inline void pd2_audio_read(struct pd2_audio_ring *ring, float *out, uint32_t count)
{
    const float *data = pd2_audio_data(ring);
    uint64_t pos = __atomic_load_n(&ring->read_pos, __ATOMIC_RELAXED);
    uint32_t at = (uint32_t)(pos & (PD2_AUDIO_FRAMES - 1));
    uint32_t first = count < PD2_AUDIO_FRAMES - at ? count : PD2_AUDIO_FRAMES - at;
    memcpy(out, data + at * PD2_AUDIO_CHANNELS, first * PD2_AUDIO_CHANNELS * sizeof(float));
    memcpy(out + first * PD2_AUDIO_CHANNELS, data, (count - first) * PD2_AUDIO_CHANNELS * sizeof(float));
    __atomic_store_n(&ring->read_pos, pos + count, __ATOMIC_RELEASE);
}

#endif
