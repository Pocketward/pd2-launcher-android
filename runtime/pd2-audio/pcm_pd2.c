/*
 * pcm_pd2.c: the ALSA PCM type "pd2", the game's sound output on Android. An ALSA I/O plugin
 * (alsa-lib's ioplug) that writes into the ring the Android app plays (pd2_audio_ring.h), so
 * Wine's winealsa.drv hands the game's DirectSound mix straight to the app: no sound server, no
 * pipe. Playback only, float stereo at 22.05 kHz (Diablo II's rate, so DirectSound mixes without
 * resampling; the app resamples to the phone's rate). The hardware position is the app's read
 * position plus the cushion the app asks for (pd2_audio_ring.h), so the frames leave as the
 * phone plays them.
 *
 *   pcm.!default { type pd2 ring "<path of the app's ring file>" period <frames> }
 *   pcm_type.pd2 { lib "<path of this library>" }
 *
 * period (optional, 16 to 1024 frames, 64 when not given) is the only ALSA period the PCM offers.
 * Wine's DirectSound keeps three times winealsa's latency queued (its ALSA period + its own 10 ms
 * period + 4 ms), so the period sets how much sound waits ahead of the phone, and how long a
 * stall of Wine's mixing can be before the phone runs out: 64 frames gives 50.6 ms.
 *
 * One stream writes at a time. A stream of another process is refused at prepare (EBUSY) while
 * the process that holds the ring lives. In the process that holds it, the stream prepared last
 * holds it: DirectSound opens its new client before it releases the old one when it opens the
 * device again (dsound primary.c, DSOUND_ReopenDevice). An older stream stays still meanwhile
 * (its position does not move, what it writes is dropped); when the newer one goes, the ring goes
 * back to the older one, so a short second stream (a Wine system sound, for example) does not
 * take the game's sound for good. What the older stream wrote meanwhile counts as played.
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <alsa/asoundlib.h>
#include <alsa/pcm_external.h>

#include "pd2_audio_ring.h"

/* ALSA's period when the configuration gives none: 2.9 ms. Wine still writes every 10 ms. */
#define PERIOD_FRAMES 64
#define FRAME_BYTES (PD2_AUDIO_CHANNELS * sizeof(float))

/* A stream. Its fields after ring change only under [lock]. */
struct pd2 {
    snd_pcm_ioplug_t io;
    struct pd2_audio_ring *ring;
    int holds;         /* the ring is this stream's */
    int waits;         /* a newer stream of this process holds the ring; it comes back later */
    int started;       /* started, and not stopped since */
    struct pd2 *below; /* the older stream that waits for the ring to come back, if any */
    snd_pcm_uframes_t boundary;
    uint64_t sent;     /* frames the stream gave since its prepare, the dropped ones too */
    uint64_t base;     /* its position when the ring became its own: what it gave before is played */
    uint64_t reported; /* the ring's position given last (pd2_audio_hw), from base */
};

/* The stream of this process that holds the ring (the top of the waiting ones), if one does. */
static struct pd2 *holder;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

/* [pd2] takes the ring from where its stream starts: nothing it gave before is played. */
static void take_ring(struct pd2 *pd2)
{
    pd2->holds = 1;
    pd2->waits = 0;
    pd2->base = pd2->sent;
    pd2->reported = 0;
    pd2_audio_prepare(pd2->ring);
    if (pd2->started)
        pd2_audio_start(pd2->ring);
}

/* At prepare: the stream's count starts again; the ring is its own unless another process holds
 * it. A waiting stream keeps waiting. */
static int pd2_claim(struct pd2 *pd2)
{
    int err = 0;
    pthread_mutex_lock(&lock);
    pd2->sent = 0;
    pd2->base = 0;
    pd2->reported = 0;
    pd2->started = 0;
    if (pd2->holds) {
        pd2_audio_prepare(pd2->ring);
    } else if (!pd2->waits) {
        int self = (int)getpid();
        int owner = __atomic_load_n(&pd2->ring->owner, __ATOMIC_ACQUIRE);
        while (owner != self) {
            if (owner != 0 && !(kill(owner, 0) < 0 && errno == ESRCH)) {
                err = -EBUSY;
                break;
            }
            if (__atomic_compare_exchange_n(&pd2->ring->owner, &owner, self, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE))
                break;
        }
        if (!err) {
            if (holder) {
                holder->holds = 0;
                holder->waits = 1;
            }
            pd2->below = holder;
            holder = pd2;
            take_ring(pd2);
        }
    }
    pthread_mutex_unlock(&lock);
    return err;
}

/* At hw_free and close: the ring goes back to the stream below, else it is free. */
static void pd2_release(struct pd2 *pd2)
{
    pthread_mutex_lock(&lock);
    if (holder == pd2) {
        holder = pd2->below;
        if (holder) {
            take_ring(holder);
        } else {
            int self = (int)getpid();
            pd2_audio_stop(pd2->ring);
            __atomic_compare_exchange_n(&pd2->ring->owner, &self, 0, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
        }
    } else {
        for (struct pd2 *p = holder; p; p = p->below) {
            if (p->below == pd2) {
                p->below = pd2->below;
                break;
            }
        }
    }
    pd2->holds = 0;
    pd2->waits = 0;
    pd2->started = 0;
    pd2->below = NULL;
    pthread_mutex_unlock(&lock);
}

static int pd2_start(snd_pcm_ioplug_t *io)
{
    struct pd2 *pd2 = io->private_data;
    int err = 0;
    pthread_mutex_lock(&lock);
    if (pd2->holds || pd2->waits) {
        pd2->started = 1;
        if (pd2->holds)
            pd2_audio_start(pd2->ring);
    } else {
        err = -EBADFD;
    }
    pthread_mutex_unlock(&lock);
    return err;
}

static int pd2_stop(snd_pcm_ioplug_t *io)
{
    struct pd2 *pd2 = io->private_data;
    pthread_mutex_lock(&lock);
    pd2->started = 0;
    if (pd2->holds)
        pd2_audio_stop(pd2->ring);
    pthread_mutex_unlock(&lock);
    return 0;
}

static snd_pcm_sframes_t pd2_pointer(snd_pcm_ioplug_t *io)
{
    struct pd2 *pd2 = io->private_data;
    uint64_t position = 0;
    pthread_mutex_lock(&lock);
    if (pd2->holds)
        position = pd2->base + pd2_audio_hw(pd2->ring, &pd2->reported);
    else if (pd2->waits)
        position = pd2->base + pd2->reported; /* still while it waits */
    pthread_mutex_unlock(&lock);
    return (snd_pcm_sframes_t)(position % pd2->boundary);
}

static snd_pcm_sframes_t pd2_transfer(snd_pcm_ioplug_t *io, const snd_pcm_channel_area_t *areas,
                                      snd_pcm_uframes_t offset, snd_pcm_uframes_t size)
{
    struct pd2 *pd2 = io->private_data;
    snd_pcm_sframes_t done = (snd_pcm_sframes_t)size;
    pthread_mutex_lock(&lock);
    if (pd2->holds) {
        const float *frames = (const float *)((const char *)areas->addr + (areas->first + areas->step * offset) / 8);
        pd2_audio_write(pd2->ring, frames, (uint32_t)size);
        pd2->sent += size;
    } else if (pd2->waits) {
        /* Dropped while it waits, with no error for its writer. */
        pd2->sent += size;
    } else {
        done = -EBADFD;
    }
    pthread_mutex_unlock(&lock);
    return done;
}

static int pd2_prepare(snd_pcm_ioplug_t *io)
{
    struct pd2 *pd2 = io->private_data;
    snd_pcm_sw_params_t *sw;
    snd_pcm_sw_params_alloca(&sw);
    if (snd_pcm_sw_params_current(io->pcm, sw) < 0 || snd_pcm_sw_params_get_boundary(sw, &pd2->boundary) < 0 ||
        pd2->boundary == 0)
        pd2->boundary = io->buffer_size;
    return pd2_claim(pd2);
}

static int pd2_hw_free(snd_pcm_ioplug_t *io)
{
    pd2_release(io->private_data);
    return 0;
}

static int pd2_close(snd_pcm_ioplug_t *io)
{
    struct pd2 *pd2 = io->private_data;
    pd2_release(pd2);
    if (io->poll_fd >= 0)
        close(io->poll_fd);
    munmap(pd2->ring, PD2_AUDIO_FILE_SIZE);
    free(pd2);
    return 0;
}

static const snd_pcm_ioplug_callback_t pd2_callback = {
    .start = pd2_start,
    .stop = pd2_stop,
    .pointer = pd2_pointer,
    .transfer = pd2_transfer,
    .close = pd2_close,
    .hw_free = pd2_hw_free,
    .prepare = pd2_prepare,
};

static int pd2_constraints(snd_pcm_ioplug_t *io, unsigned int period)
{
    static const unsigned int access[] = {SND_PCM_ACCESS_RW_INTERLEAVED};
    static const unsigned int format[] = {SND_PCM_FORMAT_FLOAT_LE};
    int err;
    if ((err = snd_pcm_ioplug_set_param_list(io, SND_PCM_IOPLUG_HW_ACCESS, 1, access)) < 0 ||
        (err = snd_pcm_ioplug_set_param_list(io, SND_PCM_IOPLUG_HW_FORMAT, 1, format)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_CHANNELS, PD2_AUDIO_CHANNELS, PD2_AUDIO_CHANNELS)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_RATE, PD2_AUDIO_RATE, PD2_AUDIO_RATE)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_PERIOD_BYTES, period * FRAME_BYTES,
                                               period * FRAME_BYTES)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_PERIODS, 2, PD2_AUDIO_FRAMES / 2 / period)) < 0 ||
        (err = snd_pcm_ioplug_set_param_minmax(io, SND_PCM_IOPLUG_HW_BUFFER_BYTES, 2 * period * FRAME_BYTES,
                                               PD2_AUDIO_FRAMES / 2 * FRAME_BYTES)) < 0)
        return err;
    return 0;
}

SND_PCM_PLUGIN_DEFINE_FUNC(pd2)
{
    snd_config_iterator_t i, next;
    const char *path = NULL;
    long period = PERIOD_FRAMES;
    struct pd2 *pd2;
    struct stat st;
    void *map;
    int fd, err;

    (void)root;
    snd_config_for_each(i, next, conf) {
        snd_config_t *n = snd_config_iterator_entry(i);
        const char *id;
        if (snd_config_get_id(n, &id) < 0)
            continue;
        if (!strcmp(id, "comment") || !strcmp(id, "type") || !strcmp(id, "hint"))
            continue;
        if (!strcmp(id, "ring")) {
            if (snd_config_get_string(n, &path) < 0) {
                SNDERR("pd2: ring must be a path");
                return -EINVAL;
            }
            continue;
        }
        if (!strcmp(id, "period")) {
            if (snd_config_get_integer(n, &period) < 0 || period < 16 || period > 1024) {
                SNDERR("pd2: period must be 16 to 1024 frames");
                return -EINVAL;
            }
            continue;
        }
        SNDERR("pd2: unknown field %s", id);
        return -EINVAL;
    }
    if (!path) {
        SNDERR("pd2: no ring");
        return -EINVAL;
    }
    if (stream != SND_PCM_STREAM_PLAYBACK)
        return -ENODEV;

    fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0)
        return -errno;
    if (fstat(fd, &st) < 0 || (uint64_t)st.st_size < PD2_AUDIO_FILE_SIZE) {
        close(fd);
        return -ENODEV;
    }
    map = mmap(NULL, PD2_AUDIO_FILE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (map == MAP_FAILED)
        return -errno;
    if (!pd2_audio_valid(map, (uint64_t)st.st_size)) {
        munmap(map, PD2_AUDIO_FILE_SIZE);
        return -ENODEV;
    }

    pd2 = calloc(1, sizeof(*pd2));
    if (!pd2) {
        munmap(map, PD2_AUDIO_FILE_SIZE);
        return -ENOMEM;
    }
    pd2->ring = map;
    pd2->io.version = SND_PCM_IOPLUG_VERSION;
    pd2->io.name = "PD2 Launcher game sound";
    pd2->io.callback = &pd2_callback;
    pd2->io.private_data = pd2;
    pd2->io.flags = SND_PCM_IOPLUG_FLAG_BOUNDARY_WA;
    /* Wine writes on its own timer and never waits in poll; an eventfd is always writable. */
    pd2->io.poll_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    pd2->io.poll_events = POLLOUT;
    if (pd2->io.poll_fd < 0) {
        err = -errno;
        munmap(map, PD2_AUDIO_FILE_SIZE);
        free(pd2);
        return err;
    }

    err = snd_pcm_ioplug_create(&pd2->io, name, stream, mode);
    if (err < 0) {
        close(pd2->io.poll_fd);
        munmap(map, PD2_AUDIO_FILE_SIZE);
        free(pd2);
        return err;
    }
    err = pd2_constraints(&pd2->io, (unsigned int)period);
    if (err < 0) {
        snd_pcm_ioplug_delete(&pd2->io);
        return err;
    }
    *pcmp = pd2->io.pcm;
    return 0;
}

SND_PCM_PLUGIN_SYMBOL(pd2);
