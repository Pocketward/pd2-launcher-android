/*
 * The game's sound on Android (GameAudio.kt): plays the ring that the game's Wine writes through
 * ALSA (runtime/pd2-audio: pd2_audio_ring.h, pcm_pd2.c) on a low-latency AAudio stream at the
 * phone's own rate and burst, so the stream gets Android's fast mixer path. The data callback takes
 * what the ring holds, resamples it from 22.05 kHz (game_audio_resampler.c) and plays silence for
 * what is missing; Wine's DirectSound sees the frames leave as they are played (the ring's read
 * position, plus a cushion, is its ALSA hardware position). A new stream of the game starts
 * playing once the ring holds two callbacks' input; a short ring while it plays raises the
 * cushion, and calm lowers it again (game_audio_margin.h). Muted means silence while the ring
 * still drains. The buffer starts at two bursts and grows by one burst after an underrun of the
 * stream. A lost stream is opened again on a thread of its own; while there is none, that thread
 * drains the ring at the game's rate (game_audio_restart.h).
 */
#include <aaudio/AAudio.h>
#include <android/log.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <math.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "game_audio_margin.h"
#include "game_audio_resampler.h"
#include "game_audio_restart.h"
#include "pd2_audio_ring.h"

#define TAG "pd2-audio"
#define LOG(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

/* The stream's buffer: two bursts at first, at most eight. */
#define START_BURSTS 2
#define MAX_BURSTS 8
/* The restarter's drain: its scratch buffer, and how often it drains while it waits. */
#define DRAIN_FRAMES 512
#define DRAIN_SLICE_NS 20000000LL /* 20 ms */

struct game_audio {
    struct pd2_audio_ring *ring;
    pthread_mutex_t lock; /* the stream's opening and closing */
    AAudioStream *stream;
    struct ga_resampler resampler;
    float *input;
    uint32_t input_frames;
    int32_t rate;
    int32_t burst;
    int32_t bursts;
    int32_t xruns;
    int muted;
    int primed;          /* the writer's stream runs and the ring held enough to start */
    struct ga_margin margin;
    uint32_t cushion;    /* the cushion the ring asks for (game_audio_margin.h) */
    int stopping;
    int restart;         /* an error for the restarter to handle (on_error sets it without the lock) */
    pthread_t restarter;
    pthread_cond_t wake;
    /* The restarter's: the drain while there is no stream (game_audio_restart.h), from the time
     * the stream closed (drain_last) on, the time spent opening it again included. */
    int64_t drain_last;
    uint64_t drain_carry;
    uint64_t drained;
    float drain[2 * DRAIN_FRAMES];
    /* Statistics, read by game_audio_stats. */
    uint64_t frames_read __attribute__((aligned(8)));
    uint64_t frames_played __attribute__((aligned(8)));
    uint64_t dry __attribute__((aligned(8)));
    uint32_t peak;
    uint32_t restarts;
};

static int64_t now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static aaudio_data_callback_result_t on_audio(AAudioStream *stream, void *user, void *data, int32_t frames)
{
    struct game_audio *ga = user;
    float *out = data;
    uint32_t need = ga_resampler_needed(&ga->resampler, (uint32_t)frames);
    if (need > ga->input_frames) {
        /* A callback longer than the stream's capacity: never expected; play silence. */
        memset(out, 0, (size_t)frames * 2 * sizeof(float));
        return AAUDIO_CALLBACK_RESULT_CONTINUE;
    }
    uint32_t ready = pd2_audio_readable(ga->ring);
    int64_t now = now_ns();
    struct ga_take t = ga_margin_take(ga->primed, __atomic_load_n(&ga->ring->running, __ATOMIC_ACQUIRE), ready, need);
    ga->primed = t.primed;
    if (t.started)
        ga_margin_start(&ga->margin, now);
    uint32_t take = t.take;
    if (take)
        pd2_audio_read(ga->ring, ga->input, take);
    /* Short while the writer's stream plays: silence for the rest, and a bigger cushion. */
    if (take < need)
        memset(ga->input + 2 * take, 0, (size_t)(need - take) * 2 * sizeof(float));
    if (t.dry) {
        __atomic_add_fetch(&ga->dry, 1, __ATOMIC_RELAXED);
        __atomic_add_fetch(&ga->ring->underruns, 1, __ATOMIC_RELAXED);
    }
    uint32_t cushion = ga_margin_update(&ga->margin, t.dry, now);
    if (cushion != __atomic_load_n(&ga->cushion, __ATOMIC_RELAXED)) {
        __atomic_store_n(&ga->cushion, cushion, __ATOMIC_RELAXED);
        pd2_audio_set_cushion(ga->ring, cushion);
    }
    ga_resampler_process(&ga->resampler, ga->input, out, (uint32_t)frames);
    if (__atomic_load_n(&ga->muted, __ATOMIC_RELAXED)) {
        memset(out, 0, (size_t)frames * 2 * sizeof(float));
    } else {
        float peak = 0;
        for (int32_t i = 0; i < 2 * frames; i++) {
            float v = fabsf(out[i]);
            if (v > peak)
                peak = v;
        }
        uint32_t p = peak >= 1.0f ? 32768u : (uint32_t)(peak * 32768.0f);
        if (p > __atomic_load_n(&ga->peak, __ATOMIC_RELAXED))
            __atomic_store_n(&ga->peak, p, __ATOMIC_RELAXED);
    }
    __atomic_add_fetch(&ga->frames_read, take, __ATOMIC_RELAXED);
    __atomic_add_fetch(&ga->frames_played, (uint64_t)frames, __ATOMIC_RELAXED);

    /* Underruns of the stream itself (the callback came late): one burst more. */
    int32_t xruns = AAudioStream_getXRunCount(stream);
    if (xruns > ga->xruns) {
        ga->xruns = xruns;
        if (ga->bursts < MAX_BURSTS) {
            ga->bursts++;
            AAudioStream_setBufferSizeInFrames(stream, ga->bursts * __atomic_load_n(&ga->burst, __ATOMIC_RELAXED));
        }
    }
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

static void on_error(AAudioStream *stream, void *user, aaudio_result_t error)
{
    (void)stream;
    struct game_audio *ga = user;
    /* The stream is gone (another output device, for example): it is opened again on the
     * restarter thread, never in this callback. No lock here: the restarter may hold it while it
     * closes a stream, and closing waits for this callback. */
    __atomic_store_n(&ga->restart, error ? error : AAUDIO_ERROR_DISCONNECTED, __ATOMIC_RELEASE);
    pthread_cond_signal(&ga->wake);
}

/* Opens and starts the stream; called with the lock held. */
static int open_stream(struct game_audio *ga)
{
    AAudioStreamBuilder *builder;
    if (AAudio_createStreamBuilder(&builder) != AAUDIO_OK)
        return -1;
    AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_OUTPUT);
    AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_FLOAT);
    AAudioStreamBuilder_setChannelCount(builder, 2);
    AAudioStreamBuilder_setUsage(builder, AAUDIO_USAGE_GAME);
    AAudioStreamBuilder_setContentType(builder, AAUDIO_CONTENT_TYPE_MUSIC);
    AAudioStreamBuilder_setDataCallback(builder, on_audio, ga);
    AAudioStreamBuilder_setErrorCallback(builder, on_error, ga);
    AAudioStream *stream = NULL;
    aaudio_result_t result = AAudioStreamBuilder_openStream(builder, &stream);
    AAudioStreamBuilder_delete(builder);
    if (result != AAUDIO_OK) {
        LOG("audio: no output stream: %s", AAudio_convertResultToText(result));
        return -1;
    }
    int32_t rate = AAudioStream_getSampleRate(stream);
    int32_t burst = AAudioStream_getFramesPerBurst(stream);
    int32_t capacity = AAudioStream_getBufferCapacityInFrames(stream);
    if (rate <= 0 || burst <= 0 || capacity <= 0) {
        AAudioStream_close(stream);
        return -1;
    }
    if (rate != __atomic_load_n(&ga->rate, __ATOMIC_RELAXED)) {
        ga_resampler_free(&ga->resampler);
        if (ga_resampler_init(&ga->resampler, PD2_AUDIO_RATE, (uint32_t)rate) < 0) {
            AAudioStream_close(stream);
            return -1;
        }
    }
    /* Room for the input of the longest callback the stream can ask for. */
    uint32_t frames = (uint32_t)((uint64_t)capacity * PD2_AUDIO_RATE / (uint32_t)rate) + 4;
    if (frames > ga->input_frames) {
        float *input = realloc(ga->input, (size_t)frames * 2 * sizeof(float));
        if (!input) {
            AAudioStream_close(stream);
            return -1;
        }
        ga->input = input;
        ga->input_frames = frames;
    }
    __atomic_store_n(&ga->rate, rate, __ATOMIC_RELAXED);
    __atomic_store_n(&ga->burst, burst, __ATOMIC_RELAXED);
    ga->bursts = START_BURSTS;
    ga->xruns = 0;
    AAudioStream_setBufferSizeInFrames(stream, START_BURSTS * burst);
    ga->stream = stream;
    result = AAudioStream_requestStart(stream);
    if (result != AAUDIO_OK) {
        LOG("audio: the output stream did not start: %s", AAudio_convertResultToText(result));
        AAudioStream_close(stream);
        ga->stream = NULL;
        return -1;
    }
    LOG("audio: output stream at %d Hz, burst %d frames, buffer %d of %d frames, %s, %s", rate, burst,
        AAudioStream_getBufferSizeInFrames(stream), capacity,
        AAudioStream_getPerformanceMode(stream) == AAUDIO_PERFORMANCE_MODE_LOW_LATENCY ? "low latency" : "not low latency",
        AAudioStream_getSharingMode(stream) == AAUDIO_SHARING_MODE_EXCLUSIVE ? "exclusive" : "shared");
    return 0;
}

/* Waits on the restarter's condition (on CLOCK_MONOTONIC, game_audio_start) until [until_ns] at
 * the latest; called with the lock held. */
static void wait_until(struct game_audio *ga, int64_t until_ns)
{
    if (until_ns <= now_ns())
        return;
    struct timespec until = {(time_t)(until_ns / 1000000000LL), (long)(until_ns % 1000000000LL)};
    pthread_cond_timedwait(&ga->wake, &ga->lock, &until);
}

/* While there is no stream (called with the lock held): the frames due at the game's rate since
 * the last drain, or since the stream closed, are taken from the ring and dropped. */
static void drain_now(struct game_audio *ga)
{
    if (ga->stream)
        return;
    int64_t now = now_ns();
    uint32_t due = ga_drain_due(&ga->drain_carry, now - ga->drain_last, PD2_AUDIO_RATE);
    ga->drain_last = now;
    uint32_t taken = ga_drain(ga->ring, ga->drain, DRAIN_FRAMES, due);
    ga->drained += taken;
    __atomic_add_fetch(&ga->frames_read, taken, __ATOMIC_RELAXED);
}

/*
 * Waits [seconds], or less when the player stops or an error comes (called with the lock held).
 * While there is no stream, nothing else reads the ring: every 20 ms the frames due at the game's
 * rate are taken and dropped (drain_now), so DirectSound's play position keeps moving.
 */
static void wait_draining(struct game_audio *ga, int seconds)
{
    int64_t end = now_ns() + (int64_t)seconds * 1000000000LL;
    while (!ga->stopping && !__atomic_load_n(&ga->restart, __ATOMIC_ACQUIRE)) {
        drain_now(ga);
        int64_t now = now_ns();
        if (now >= end)
            break;
        wait_until(ga, ga->stream || end - now < DRAIN_SLICE_NS ? end : now + DRAIN_SLICE_NS);
    }
}

static void *restarter(void *arg)
{
    struct game_audio *ga = arg;
    int failures = 0;     /* tries in a row that gave no stream that stayed up (game_audio_restart.h) */
    int64_t up_since = 0; /* when the stream in use opened here; 0: the first one, or none */
    pthread_mutex_lock(&ga->lock);
    while (!ga->stopping) {
        int error = __atomic_exchange_n(&ga->restart, 0, __ATOMIC_ACQ_REL);
        if (!error) {
            /* A timed wait: the error callback signals without the lock (see on_error). */
            wait_draining(ga, 1);
            continue;
        }
        /* The stream that went: only one that stayed up long enough ends a run of failures. */
        if (up_since) {
            failures = ga_restart_failures(failures, now_ns() - up_since);
            up_since = 0;
        }
        if (ga->stream) {
            /* The first loss, then every tenth of a run that goes again soon after it opens. */
            if (failures % 10 == 0)
                LOG("audio: output stream lost (%s, %d in a row), opening it again", AAudio_convertResultToText(error), failures + 1);
            AAudioStream_close(ga->stream);
            ga->stream = NULL;
            ga->drain_last = now_ns();
        }
        /* After failures: again after 1, 2 and 4 s, then every 10 s, with the ring drained meanwhile. */
        wait_draining(ga, ga_restart_wait(failures));
        if (ga->stopping)
            break;
        ga->restarts++;
        drain_now(ga);
        int opened = open_stream(ga);
        /* A try that failed: what came due while it tried (with a stream, its callback reads). */
        drain_now(ga);
        if (opened == 0) {
            up_since = now_ns();
            if (ga->drained) {
                LOG("audio: %llu frames dropped while there was no output stream", (unsigned long long)ga->drained);
                ga->drained = 0;
            }
            continue;
        }
        /* No output now (the device changes). */
        failures++;
        if (failures % 10 == 1)
            LOG("audio: no output stream yet (%d tries)", failures);
        __atomic_store_n(&ga->restart, AAUDIO_ERROR_DISCONNECTED, __ATOMIC_RELEASE);
    }
    pthread_mutex_unlock(&ga->lock);
    return NULL;
}

static struct game_audio *game_audio_start(const char *path)
{
    /* A new file each time: a writer of an earlier session never shares it. */
    unlink(path);
    int fd = open(path, O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (fd < 0) {
        LOG("audio: no ring file %s: %s", path, strerror(errno));
        return NULL;
    }
    if (ftruncate(fd, PD2_AUDIO_FILE_SIZE) < 0) {
        LOG("audio: ring file %s: %s", path, strerror(errno));
        close(fd);
        return NULL;
    }
    void *map = mmap(NULL, PD2_AUDIO_FILE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (map == MAP_FAILED) {
        LOG("audio: ring file %s: %s", path, strerror(errno));
        return NULL;
    }
    struct game_audio *ga = calloc(1, sizeof(*ga));
    if (!ga) {
        munmap(map, PD2_AUDIO_FILE_SIZE);
        return NULL;
    }
    ga->ring = map;
    pd2_audio_init(ga->ring);
    ga_margin_init(&ga->margin);
    pthread_mutex_init(&ga->lock, NULL);
    /* The restarter's waits run on CLOCK_MONOTONIC (wait_until): a change of the wall clock does
     * not move them. */
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&ga->wake, &attr);
    pthread_condattr_destroy(&attr);
    pthread_mutex_lock(&ga->lock);
    int opened = open_stream(ga);
    pthread_mutex_unlock(&ga->lock);
    if (opened < 0 || pthread_create(&ga->restarter, NULL, restarter, ga) != 0) {
        if (ga->stream)
            AAudioStream_close(ga->stream);
        ga_resampler_free(&ga->resampler);
        free(ga->input);
        munmap(map, PD2_AUDIO_FILE_SIZE);
        free(ga);
        return NULL;
    }
    return ga;
}

static void game_audio_stop(struct game_audio *ga)
{
    pthread_mutex_lock(&ga->lock);
    ga->stopping = 1;
    pthread_cond_signal(&ga->wake);
    pthread_mutex_unlock(&ga->lock);
    pthread_join(ga->restarter, NULL);
    if (ga->stream) {
        AAudioStream_requestStop(ga->stream);
        AAudioStream_close(ga->stream);
    }
    ga_resampler_free(&ga->resampler);
    free(ga->input);
    munmap(ga->ring, PD2_AUDIO_FILE_SIZE);
    pthread_mutex_destroy(&ga->lock);
    pthread_cond_destroy(&ga->wake);
    free(ga);
}

JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_GameSound_nativeStart(JNIEnv *env, jclass cls, jstring ring)
{
    (void)cls;
    const char *path = (*env)->GetStringUTFChars(env, ring, NULL);
    if (!path)
        return 0;
    struct game_audio *ga = game_audio_start(path);
    (*env)->ReleaseStringUTFChars(env, ring, path);
    return (jlong)(intptr_t)ga;
}

JNIEXPORT void JNICALL Java_com_pd2launcher_app_GameSound_nativeSetMuted(JNIEnv *env, jclass cls, jlong handle, jboolean muted)
{
    (void)env;
    (void)cls;
    struct game_audio *ga = (struct game_audio *)(intptr_t)handle;
    if (ga)
        __atomic_store_n(&ga->muted, muted ? 1 : 0, __ATOMIC_RELAXED);
}

/*
 * [frames read from the ring, callbacks that found it short while the game's stream ran, the
 * stream's own underruns, buffer frames, burst frames, rate, loudest sample since the last call
 * (0 to 32768), the output delay in microseconds (-1 when unknown), stream restarts, the cushion in
 * frames].
 */
JNIEXPORT jlongArray JNICALL Java_com_pd2launcher_app_GameSound_nativeStats(JNIEnv *env, jclass cls, jlong handle)
{
    (void)cls;
    struct game_audio *ga = (struct game_audio *)(intptr_t)handle;
    jlong values[10] = {0};
    if (ga) {
        pthread_mutex_lock(&ga->lock);
        AAudioStream *stream = ga->stream;
        jlong delay = -1;
        int32_t buffer = 0, xruns = 0;
        if (stream) {
            int64_t position, time;
            buffer = AAudioStream_getBufferSizeInFrames(stream);
            xruns = AAudioStream_getXRunCount(stream);
            int32_t rate = __atomic_load_n(&ga->rate, __ATOMIC_RELAXED);
            if (AAudioStream_getTimestamp(stream, CLOCK_MONOTONIC, &position, &time) == AAUDIO_OK && rate > 0) {
                int64_t written = AAudioStream_getFramesWritten(stream);
                delay = (jlong)(((written - position) * 1000000LL) / rate - (now_ns() - time) / 1000);
            }
        }
        values[0] = (jlong)__atomic_load_n(&ga->frames_read, __ATOMIC_RELAXED);
        values[1] = (jlong)__atomic_load_n(&ga->dry, __ATOMIC_RELAXED);
        values[2] = xruns;
        values[3] = buffer;
        values[4] = __atomic_load_n(&ga->burst, __ATOMIC_RELAXED);
        values[5] = __atomic_load_n(&ga->rate, __ATOMIC_RELAXED);
        values[6] = (jlong)__atomic_exchange_n(&ga->peak, 0, __ATOMIC_RELAXED);
        values[7] = delay;
        values[8] = ga->restarts;
        values[9] = __atomic_load_n(&ga->cushion, __ATOMIC_RELAXED);
        pthread_mutex_unlock(&ga->lock);
    }
    jlongArray array = (*env)->NewLongArray(env, 10);
    if (array)
        (*env)->SetLongArrayRegion(env, array, 0, 10, values);
    return array;
}

JNIEXPORT void JNICALL Java_com_pd2launcher_app_GameSound_nativeStop(JNIEnv *env, jclass cls, jlong handle)
{
    (void)env;
    (void)cls;
    struct game_audio *ga = (struct game_audio *)(intptr_t)handle;
    if (ga)
        game_audio_stop(ga);
}
