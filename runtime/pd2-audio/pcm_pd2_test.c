/*
 * pcm_pd2_test.c <plugin .so> <work folder>: the pd2 ALSA plugin (pcm_pd2.c) through alsa-lib,
 * opened as Wine's winealsa.drv opens it: its one format, rate and period; writes that never block;
 * the hardware position following the reader and its cushion; a newer stream of the same process
 * taking the ring over, and the ring going back to the older stream when the newer one goes;
 * another process refused while the holder lives, and let in once it is gone.
 * pd2audio.sh builds it for the build machine and runs it. Exit 0 when every check passes.
 */
#include <alsa/asoundlib.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "pd2_audio_ring.h"

static int failures;

#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                \
        }                                                              \
    } while (0)

static struct pd2_audio_ring *ring;

static double now(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

/* Opens "default" and sets it up as winealsa.drv does (alsa_create_stream); 0 or an error. */
static int open_like_winealsa(snd_pcm_t **pcm, snd_pcm_uframes_t *period, snd_pcm_uframes_t *buffer)
{
    int err = snd_pcm_open(pcm, "default", SND_PCM_STREAM_PLAYBACK, SND_PCM_NONBLOCK);
    if (err < 0)
        return err;
    snd_pcm_hw_params_t *hw;
    snd_pcm_hw_params_alloca(&hw);
    snd_pcm_hw_params_any(*pcm, hw);
    snd_pcm_hw_params_set_access(*pcm, hw, SND_PCM_ACCESS_RW_INTERLEAVED);
    snd_pcm_hw_params_set_format(*pcm, hw, SND_PCM_FORMAT_FLOAT_LE);
    snd_pcm_hw_params_set_rate_resample(*pcm, hw, 0);
    snd_pcm_hw_params_set_rate(*pcm, hw, PD2_AUDIO_RATE, 0);
    snd_pcm_hw_params_set_channels(*pcm, hw, 2);
    unsigned int us = 10000;
    snd_pcm_hw_params_set_period_time_near(*pcm, hw, &us, NULL);
    snd_pcm_uframes_t want = 4 * 220;
    snd_pcm_hw_params_set_buffer_size_near(*pcm, hw, &want);
    if ((err = snd_pcm_hw_params(*pcm, hw)) < 0)
        goto fail;
    snd_pcm_hw_params_get_period_size(hw, period, NULL);
    snd_pcm_hw_params_get_buffer_size(hw, buffer);
    snd_pcm_sw_params_t *sw;
    snd_pcm_sw_params_alloca(&sw);
    snd_pcm_sw_params_current(*pcm, sw);
    snd_pcm_sw_params_set_start_threshold(*pcm, sw, 1);
    snd_pcm_sw_params_set_stop_threshold(*pcm, sw, *buffer);
    snd_pcm_sw_params(*pcm, sw);
    if ((err = snd_pcm_prepare(*pcm)) < 0)
        goto fail;
    return 0;
fail:
    snd_pcm_close(*pcm);
    *pcm = NULL;
    return err;
}

/* Writes 4 frames at a time until the stream takes no more (alsa-lib refuses a write larger than
 * what is free while less than a period is free): frames written. */
static long fill(snd_pcm_t *pcm, float value)
{
    float data[2 * 64];
    for (int i = 0; i < 128; i++)
        data[i] = value;
    long total = 0;
    for (int i = 0; i < 4000; i++) {
        snd_pcm_sframes_t w = snd_pcm_writei(pcm, data, 4);
        if (w == -EAGAIN || w == 0)
            break;
        if (w < 0) {
            fprintf(stderr, "write: %s\n", snd_strerror((int)w));
            return -1;
        }
        total += w;
    }
    return total;
}

/* In a child process: 0 when the child's stream opened and prepared, else 1. */
static int other_process_opens(int keep)
{
    pid_t child = fork();
    if (child == 0) {
        snd_pcm_t *pcm;
        snd_pcm_uframes_t period, buffer;
        int err = open_like_winealsa(&pcm, &period, &buffer);
        if (err == 0 && !keep)
            snd_pcm_close(pcm);
        _exit(err == 0 ? 0 : 1);
    }
    int status = 0;
    waitpid(child, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 2;
}

int main(int argc, char **argv)
{
    if (argc < 3)
        return 2;
    char path[4096], config[4096];
    snprintf(path, sizeof(path), "%s/ring", argv[2]);
    snprintf(config, sizeof(config), "%s/asound.conf", argv[2]);
    FILE *f = fopen(config, "w");
    fprintf(f, "pcm.!default {\n    type pd2\n    ring \"%s\"\n    period 64\n}\npcm_type.pd2 {\n    lib \"%s\"\n}\n", path, argv[1]);
    fclose(f);
    setenv("ALSA_CONFIG_PATH", config, 1);
    unlink(path);
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL, 0600);
    if (fd < 0 || ftruncate(fd, PD2_AUDIO_FILE_SIZE) < 0)
        return 2;
    ring = mmap(NULL, PD2_AUDIO_FILE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    pd2_audio_init(ring);

    /* The one format and rate winealsa's mix format asks for, and the configured period. */
    snd_pcm_t *pcm;
    snd_pcm_uframes_t period, buffer;
    CHECK(open_like_winealsa(&pcm, &period, &buffer) == 0);
    if (!pcm)
        return 1;
    CHECK(period == 64);
    CHECK(buffer == 896);
    CHECK(ring->owner == (int)getpid());

    /* No reader: the stream fills its buffer and then refuses at once, never blocking. */
    double t0 = now();
    CHECK(fill(pcm, 1.0f) == (long)buffer);
    CHECK(now() - t0 < 0.5);
    CHECK(ring->running == 1);
    CHECK(pd2_audio_written(ring) == buffer);

    /* The reader takes 200 frames: the position follows, and 200 more fit. */
    float out[2 * 256];
    CHECK(pd2_audio_readable(ring) == buffer);
    pd2_audio_read(ring, out, 200);
    CHECK(snd_pcm_avail_update(pcm) == 200);
    CHECK(fill(pcm, 2.0f) == 200);

    /* A cushion: the stream sees that many more frames gone (never past what it wrote). */
    pd2_audio_set_cushion(ring, 300);
    CHECK(snd_pcm_avail_update(pcm) == 300);
    CHECK(fill(pcm, 3.0f) == 300);
    /* A cushion beyond half the ring counts as half the ring. */
    pd2_audio_set_cushion(ring, PD2_AUDIO_FRAMES);
    snd_pcm_sframes_t avail = snd_pcm_avail_update(pcm);
    CHECK(avail >= 0 && (uint64_t)avail <= buffer);
    pd2_audio_set_cushion(ring, 0);

    /* Another process is refused while this one holds the ring. */
    CHECK(other_process_opens(0) == 1);

    /* A newer stream of this process takes the ring over; the older one stays still. */
    snd_pcm_sframes_t before = snd_pcm_avail_update(pcm);
    snd_pcm_t *second;
    snd_pcm_uframes_t period2, buffer2;
    CHECK(open_like_winealsa(&second, &period2, &buffer2) == 0);
    if (!second)
        return 1;
    CHECK(ring->running == 0);
    CHECK(pd2_audio_written(ring) == 0);
    CHECK(pd2_audio_readable(ring) == 0);
    CHECK(snd_pcm_avail_update(pcm) == before);
    /* The older stream's writes go nowhere. */
    float data[2 * 16] = {0};
    uint64_t written = ring->write_pos;
    CHECK(snd_pcm_writei(pcm, data, 16) == 16);
    CHECK(ring->write_pos == written);
    CHECK(snd_pcm_avail_update(pcm) == before - 16);
    CHECK(fill(second, 4.0f) == (long)buffer2);
    CHECK(ring->running == 1);
    CHECK(pd2_audio_readable(ring) == buffer2);
    pd2_audio_read(ring, out, 10);
    CHECK(out[0] == 4.0f);

    /* The newer stream goes: the ring goes back to the older one, which plays on (it was
     * started), with what it wrote meanwhile counted as played. */
    snd_pcm_close(second);
    CHECK(ring->owner == (int)getpid());
    CHECK(ring->running == 1);
    CHECK(pd2_audio_written(ring) == 0);
    CHECK(snd_pcm_avail_update(pcm) == (snd_pcm_sframes_t)buffer);
    CHECK(fill(pcm, 5.0f) == (long)buffer);
    CHECK(pd2_audio_readable(ring) == buffer);
    pd2_audio_read(ring, out, 10);
    CHECK(out[0] == 5.0f);
    CHECK(snd_pcm_avail_update(pcm) == 10);

    /* The older stream goes first: the newer one keeps the ring. */
    CHECK(open_like_winealsa(&second, &period2, &buffer2) == 0);
    if (!second)
        return 1;
    snd_pcm_close(pcm);
    pcm = NULL;
    CHECK(ring->owner == (int)getpid());
    CHECK(fill(second, 6.0f) == (long)buffer2);
    CHECK(ring->running == 1);
    CHECK(pd2_audio_readable(ring) == buffer2);
    pd2_audio_read(ring, out, 10);
    CHECK(out[0] == 6.0f);
    snd_pcm_close(second);
    CHECK(ring->owner == 0);
    CHECK(ring->running == 0);

    /* With the ring free, another process gets it; one that held it and is gone does not keep it. */
    CHECK(other_process_opens(0) == 0);
    CHECK(other_process_opens(1) == 0);
    CHECK(ring->owner != 0 && ring->owner != (int)getpid());
    CHECK(open_like_winealsa(&pcm, &period, &buffer) == 0);
    CHECK(ring->owner == (int)getpid());
    if (pcm)
        snd_pcm_close(pcm);
    CHECK(ring->owner == 0);

    if (failures) {
        fprintf(stderr, "pcm_pd2_test: %d failed\n", failures);
        return 1;
    }
    printf("pcm_pd2_test: passed\n");
    return 0;
}
