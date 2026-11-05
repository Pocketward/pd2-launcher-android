/* The swapchain's present queue, with its count of the images that wait for the app's clock
 * (pc_clock_holds). pd2hybris_icd.c includes it once, after put_image and dp_present. The host test
 * app/src/test/cpp/hybris_clock_test.c includes it too, with a small swapchain of its own and stubs
 * of the GPU, the X server and the app, and runs it with real threads on a real page and futex. */

/* Under [lock]: the present thread's next job. One image more waits for the app's clock. */
static void present_queued(struct swapchain *sc, struct job job)
{
    sc->q[(sc->qhead + sc->qn) % MAX_IMAGES] = job;
    sc->qn++;
    sc->clock_waiting++;
    sc->img[job.index].state = IMG_QUEUED;
}

/* An image whose present is done, in *index (VK_SUCCESS), VK_NOT_READY at [timeout] 0, or
 * VK_TIMEOUT; the present thread frees them in order. */
static VkResult acquire_image(struct swapchain *sc, uint64_t timeout, uint32_t *index)
{
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    int forever = timeout == UINT64_MAX || timeout / 1000000000ULL > 86400;
    if (!forever) {
        deadline.tv_sec += (time_t)(timeout / 1000000000ULL);
        deadline.tv_nsec += (long)(timeout % 1000000000ULL);
        deadline.tv_sec += deadline.tv_nsec / 1000000000L;
        deadline.tv_nsec %= 1000000000L;
    }
    pthread_mutex_lock(&sc->lock);
    for (;;) {
        /* At most one presented image waits for the app's clock (present_counter.h). */
        int held = pc_clock_holds(sc->clock_waiting, timeout);
        for (uint32_t k = 0; !held && k < sc->count; k++) {
            uint32_t i = (sc->next + k) % sc->count;
            if (sc->img[i].state != IMG_FREE)
                continue;
            sc->img[i].state = IMG_ACQUIRED;
            sc->next = (i + 1) % sc->count;
            pthread_mutex_unlock(&sc->lock);
            *index = i;
            return VK_SUCCESS;
        }
        if (timeout == 0) {
            pthread_mutex_unlock(&sc->lock);
            return VK_NOT_READY;
        }
        int r = forever ? pthread_cond_wait(&sc->cond, &sc->lock)
                        : pthread_cond_timedwait(&sc->cond, &sc->lock, &deadline);
        if (r == ETIMEDOUT) {
            pthread_mutex_unlock(&sc->lock);
            return VK_TIMEOUT;
        }
    }
}

/* The present's image has passed the app's clock: an acquire that waits for that goes on. */
static void clock_passed(struct swapchain *sc)
{
    pthread_mutex_lock(&sc->lock);
    sc->clock_waiting--;
    pthread_cond_broadcast(&sc->cond);
    pthread_mutex_unlock(&sc->lock);
}

/* One present on the present thread: the GPU's copy or blit, the app's clock, then the frame to
 * the app (the direct present) or onto the X screen. */
static void present_one(struct swapchain *sc, const struct job *j)
{
    struct dev *d = sc->d;
    struct img *im = &sc->img[j->index];
    VkResult r = d->WaitForFences(d->h, 1, &im->fence, VK_TRUE, UINT64_MAX);
    if (r == VK_SUCCESS)
        r = d->ResetFences(d->h, 1, &im->fence);
    if (r != VK_SUCCESS) {
        LOGE("present: fence %d", r);
        clock_passed(sc);
        if (j->buffer >= 0) {
            pthread_mutex_lock(&sc->lock);
            sc->dir->state[j->buffer] = DP_FREE;
            pthread_cond_broadcast(&sc->cond);
            pthread_mutex_unlock(&sc->lock);
        }
        return;
    }
    /* The app's clock, in every present mode, as Mesa's software WSI waits for it (present_counter.h). */
    pc_present_clock(&sc->clock);
    clock_passed(sc);
    if (j->buffer >= 0) {
        dp_present(sc, j->buffer, j->mode);
        sc->direct_presents++;
    } else {
        if (!sc->coherent) {
            VkMappedMemoryRange mr = { .sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE, .memory = im->bufmem,
                                       .size = VK_WHOLE_SIZE };
            d->InvalidateMappedMemoryRanges(d->h, 1, &mr);
        }
        put_image(sc, im);
    }
    pc_present_counted();
    sc->presents++;
    /* The driver loads more of its libraries with its first pipelines: the fault watch's table again. */
    if (sc->presents == 1 || sc->presents == 120 || sc->presents == 1200)
        fault_watch_refresh();
    if (sc->presents == 1 || sc->presents == 1200)
        LOGD("presents %" PRIu64 ", %" PRIu64 " of them direct", sc->presents, sc->direct_presents);
}

static void *present_thread(void *arg)
{
    struct swapchain *sc = arg;
    pthread_setname_np(pthread_self(), "pd2hybris-present");
    pthread_mutex_lock(&sc->lock);
    for (;;) {
        while (!sc->qn && !sc->stop)
            pthread_cond_wait(&sc->cond, &sc->lock);
        if (!sc->qn)
            break;
        struct job j = sc->q[sc->qhead];
        pthread_mutex_unlock(&sc->lock);
        present_one(sc, &j);
        pthread_mutex_lock(&sc->lock);
        sc->qhead = (sc->qhead + 1) % MAX_IMAGES;
        sc->qn--;
        sc->img[j.index].state = IMG_FREE;
        pthread_cond_broadcast(&sc->cond);
    }
    pthread_mutex_unlock(&sc->lock);
    return NULL;
}
