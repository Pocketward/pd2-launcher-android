/*
 * A util_sync_provider for the kbase backend: DRM-syncobj semantics in user space.
 *
 * kbase has no syncobjs. PanVK takes its only vk_sync type from the device's sync provider
 * (vk_drm_syncobj_get_type_from_provider), and its CSF queue code signals and transfers syncobjs
 * the way DRM does, so this provider keeps DRM's rules:
 *  - a syncobj holds a payload or none; binary operations replace the payload, timeline
 *    operations append (point, fence) nodes to a chain;
 *  - a chain node is signalled when its fence and every earlier node's fence are;
 *  - waiting for a point needs a node at or after it (WAIT_FOR_SUBMIT waits for one to appear,
 *    WAIT_AVAILABLE waits only for that), point 0 means the whole current payload;
 *  - transfer copies the fence covering a point (point 0 of a chain: the chain head) into another
 *    syncobj's payload or chain.
 * A fence is the conjunction of conditions "this 64-bit sync object in GPU memory reached this
 * value" (one per queue ring, kbase_kmod_csf.c), or nothing (signalled). A condition whose queue
 * group hit a fatal error counts as signalled; the error is reported through the group's state.
 *
 * CPU waits check the conditions and otherwise poll() the kbase fd, which kbase wakes on GPU sync
 * updates and errors (level-triggered until read(); mali_kbase_core_linux.c kbase_poll and
 * kbase_read). One waiter at a time reads notifications; the others sleep on a condition variable.
 * Beside the kbase fd that waiter polls an eventfd, written when a payload changes (a CPU signal,
 * a submission's fence) and when another thread has read kbase's notifications, which would
 * otherwise leave the waiter asleep with nothing left for it to read. A group's fatal error is
 * passed on to the queue code (kbase_sync_set_fatal_hook), which ends the waits on its rings.
 */

#include <errno.h>
#include <inttypes.h>
#include <poll.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>

#include "drm-uapi/drm.h"

#include "util/log.h"
#include "util/macros.h"
#include "util/os_time.h"
#include "util/u_atomic.h"
#include "util/u_dynarray.h"
#include "util/u_sync_provider.h"

#include "kbase-uapi/mali_base_kernel.h"
#include "kbase-uapi/mali_kbase_ioctl.h"

#include "kbase_kmod.h"
#include "kbase_kmod_sync.h"

/* How long one waiter sleeps in poll() before it re-checks, in ms. Wake-ups come from the kernel
 * and from the eventfd; this only bounds the time to notice a condition nothing announces. */
#define KBASE_SYNC_POLL_SLICE_MS 100

#define KBASE_MAX_GROUPS 256

struct kbase_fence {
   int32_t refcnt;
   uint32_t count;
   struct kbase_fence_cond conds[];
};

struct kbase_node {
   uint64_t point;
   struct kbase_fence *fence; /* NULL: signalled */
};

struct kbase_syncobj {
   /* False: no payload (DRM's NULL fence). */
   bool has_payload;
   /* Nodes not yet known signalled, oldest first; empty with has_payload means signalled. */
   struct util_dynarray nodes;
   /* Highest chain point known signalled together with everything before it. */
   uint64_t signalled_point;
   /* Highest chain point ever added. */
   uint64_t last_point;
};

struct kbase_sync {
   int32_t refcnt;
   int fd;

   pthread_mutex_t lock;
   /* Broadcast whenever a payload changes or a waiter finished reading notifications. */
   pthread_cond_t changed;
   bool pumping;

   struct util_dynarray objs; /* struct kbase_syncobj *, indexed by handle; [0] unused */

   /* Groups with a fatal error, by kbase group handle (set by the notification reader). */
   bool group_fatal[KBASE_MAX_GROUPS];

   /* The eventfd in the polling waiter's poll set beside the kbase fd (-1: none, the waiter then
    * notices changes at the end of its slice). */
   int wake_fd;

   /* Told about each group newly marked fatal; set with atomics (kbase_sync_set_fatal_hook). */
   void (*fatal_hook)(void *data, uint8_t group);
   void *fatal_data;
};

struct kbase_sync_provider {
   struct util_sync_provider base;
   struct kbase_sync *sync;
};

static struct kbase_sync *
to_sync(struct util_sync_provider *p)
{
   return ((struct kbase_sync_provider *)p)->sync;
}

static int
fail(int err)
{
   errno = err;
   return -err;
}

/* Wakes the waiter polling the kbase fd, if any, to look again. */
static void
sync_wake(struct kbase_sync *sync)
{
   if (sync->wake_fd < 0)
      return;
   uint64_t one = 1;
   /* EAGAIN only when the counter is full, that is already non-zero: the waiter wakes anyway. */
   ssize_t ret = write(sync->wake_fd, &one, sizeof(one));
   (void)ret;
}

/* Under the lock: a payload changed. Waiters on the condition variable see it at once; the
 * polling waiter is woken through the eventfd. */
static void
sync_changed(struct kbase_sync *sync)
{
   pthread_cond_broadcast(&sync->changed);
   if (sync->pumping)
      sync_wake(sync);
}

/* --- fences ------------------------------------------------------------------------------- */

static struct kbase_fence *
fence_ref(struct kbase_fence *f)
{
   if (f)
      p_atomic_inc(&f->refcnt);
   return f;
}

static void
fence_unref(struct kbase_fence *f)
{
   if (f && p_atomic_dec_zero(&f->refcnt))
      free(f);
}

struct kbase_fence *
kbase_fence_create(const struct kbase_fence_cond *conds, uint32_t count)
{
   if (!count)
      return NULL;

   struct kbase_fence *f = malloc(sizeof(*f) + count * sizeof(conds[0]));
   if (!f)
      return NULL;

   f->refcnt = 1;
   f->count = count;
   memcpy(f->conds, conds, count * sizeof(conds[0]));
   return f;
}

void
kbase_fence_unref(struct kbase_fence *f)
{
   fence_unref(f);
}

static bool
cond_done(const struct kbase_sync *sync, const struct kbase_fence_cond *c)
{
   return __atomic_load_n(c->value_ptr, __ATOMIC_ACQUIRE) >= c->value ||
          (c->group_handle < KBASE_MAX_GROUPS &&
           __atomic_load_n(&sync->group_fatal[c->group_handle], __ATOMIC_ACQUIRE));
}

static bool
fence_done(const struct kbase_sync *sync, const struct kbase_fence *f)
{
   if (!f)
      return true;
   for (uint32_t i = 0; i < f->count; i++) {
      if (!cond_done(sync, &f->conds[i]))
         return false;
   }
   return true;
}

/* The conjunction of the fences of nodes[0..last]; NULL when all are signalled. Conditions on the
 * same sync object keep the highest value (one ring's jobs complete in order). */
static int
fence_merge(const struct kbase_sync *sync, const struct kbase_node *nodes, unsigned last,
            struct kbase_fence **out)
{
   uint32_t total = 0;
   for (unsigned i = 0; i <= last; i++) {
      if (nodes[i].fence)
         total += nodes[i].fence->count;
   }

   *out = NULL;
   if (!total)
      return 0;

   struct kbase_fence *f = malloc(sizeof(*f) + total * sizeof(f->conds[0]));
   if (!f)
      return -ENOMEM;
   f->refcnt = 1;
   f->count = 0;

   for (unsigned i = 0; i <= last; i++) {
      const struct kbase_fence *src = nodes[i].fence;
      for (uint32_t c = 0; src && c < src->count; c++) {
         if (cond_done(sync, &src->conds[c]))
            continue;
         uint32_t j;
         for (j = 0; j < f->count; j++) {
            if (f->conds[j].value_ptr == src->conds[c].value_ptr)
               break;
         }
         if (j == f->count)
            f->conds[f->count++] = src->conds[c];
         else
            f->conds[j].value = MAX2(f->conds[j].value, src->conds[c].value);
      }
   }

   if (!f->count) {
      free(f);
      return 0;
   }
   *out = f;
   return 0;
}

/* --- syncobjs ----------------------------------------------------------------------------- */

static struct kbase_syncobj *
obj_get(struct kbase_sync *sync, uint32_t handle)
{
   unsigned n = util_dynarray_num_elements(&sync->objs, struct kbase_syncobj *);
   if (!handle || handle >= n)
      return NULL;
   return *util_dynarray_element(&sync->objs, struct kbase_syncobj *, handle);
}

/* No payload, and a timeline back at point 0 (a DRM syncobj reset drops the whole chain). */
static void
obj_clear(struct kbase_syncobj *obj)
{
   util_dynarray_foreach(&obj->nodes, struct kbase_node, n)
      fence_unref(n->fence);
   util_dynarray_clear(&obj->nodes);
   obj->has_payload = false;
   obj->signalled_point = 0;
   obj->last_point = 0;
}

/* Drops leading nodes whose fences are signalled, advancing signalled_point. */
static void
obj_prune(const struct kbase_sync *sync, struct kbase_syncobj *obj)
{
   struct kbase_node *nodes = util_dynarray_begin(&obj->nodes);
   unsigned n = util_dynarray_num_elements(&obj->nodes, struct kbase_node);
   unsigned done = 0;

   while (done < n && fence_done(sync, nodes[done].fence)) {
      obj->signalled_point = MAX2(obj->signalled_point, nodes[done].point);
      fence_unref(nodes[done].fence);
      done++;
   }
   if (!done)
      return;

   memmove(nodes, nodes + done, (n - done) * sizeof(*nodes));
   obj->nodes.size -= done * sizeof(*nodes);
}

static void
obj_replace(struct kbase_syncobj *obj, struct kbase_fence *fence)
{
   obj_clear(obj);
   obj->has_payload = true;
   if (fence)
      util_dynarray_append(&obj->nodes, ((struct kbase_node){ .point = 0, .fence = fence }));
}

static void
obj_add_point(struct kbase_syncobj *obj, uint64_t point, struct kbase_fence *fence)
{
   obj->has_payload = true;
   obj->last_point = MAX2(obj->last_point, point);
   if (fence)
      util_dynarray_append(&obj->nodes, ((struct kbase_node){ .point = point, .fence = fence }));
   else if (!util_dynarray_num_elements(&obj->nodes, struct kbase_node))
      obj->signalled_point = MAX2(obj->signalled_point, point);
   else
      util_dynarray_append(&obj->nodes, ((struct kbase_node){ .point = point, .fence = NULL }));
}

/* The fence covering `point` of `obj` (a new reference, NULL when signalled). Returns false when
 * no payload covers the point yet. */
static bool
obj_fence_for(struct kbase_sync *sync, struct kbase_syncobj *obj, uint64_t point,
              struct kbase_fence **out, int *err)
{
   *out = NULL;
   *err = 0;
   if (!obj->has_payload)
      return false;

   obj_prune(sync, obj);
   const struct kbase_node *nodes = util_dynarray_begin(&obj->nodes);
   unsigned n = util_dynarray_num_elements(&obj->nodes, struct kbase_node);

   if (point == 0) {
      if (n)
         *err = fence_merge(sync, nodes, n - 1, out);
      return true;
   }

   if (point <= obj->signalled_point)
      return true;

   for (unsigned i = 0; i < n; i++) {
      if (nodes[i].point >= point) {
         *err = fence_merge(sync, nodes, i, out);
         return true;
      }
   }
   return false;
}

/* --- provider ops ------------------------------------------------------------------------- */

static int
kbase_sync_create(struct util_sync_provider *p, uint32_t flags, uint32_t *handle)
{
   struct kbase_sync *sync = to_sync(p);
   struct kbase_syncobj *obj = calloc(1, sizeof(*obj));
   if (!obj)
      return fail(ENOMEM);

   util_dynarray_init(&obj->nodes, NULL);
   if (flags & DRM_SYNCOBJ_CREATE_SIGNALED)
      obj->has_payload = true;

   pthread_mutex_lock(&sync->lock);
   unsigned n = util_dynarray_num_elements(&sync->objs, struct kbase_syncobj *);
   uint32_t h;
   for (h = 1; h < n; h++) {
      if (!*util_dynarray_element(&sync->objs, struct kbase_syncobj *, h))
         break;
   }
   if (h == n)
      util_dynarray_append(&sync->objs, obj);
   else
      *util_dynarray_element(&sync->objs, struct kbase_syncobj *, h) = obj;
   pthread_mutex_unlock(&sync->lock);

   *handle = h;
   return 0;
}

static int
kbase_sync_destroy(struct util_sync_provider *p, uint32_t handle)
{
   struct kbase_sync *sync = to_sync(p);

   pthread_mutex_lock(&sync->lock);
   struct kbase_syncobj *obj = obj_get(sync, handle);
   if (obj)
      *util_dynarray_element(&sync->objs, struct kbase_syncobj *, handle) = NULL;
   pthread_mutex_unlock(&sync->lock);

   if (!obj)
      return fail(EINVAL);

   obj_clear(obj);
   util_dynarray_fini(&obj->nodes);
   free(obj);
   return 0;
}

static int
kbase_sync_unsupported_fd(struct util_sync_provider *p, uint32_t handle, int *fd)
{
   return fail(ENOTSUP);
}

static int
kbase_sync_unsupported_import(struct util_sync_provider *p, int fd, uint32_t *handle)
{
   return fail(ENOTSUP);
}

static int
kbase_sync_unsupported_import_file(struct util_sync_provider *p, uint32_t handle, int fd)
{
   return fail(ENOTSUP);
}

static int
kbase_sync_reset(struct util_sync_provider *p, const uint32_t *handles, uint32_t count)
{
   struct kbase_sync *sync = to_sync(p);
   int ret = 0;

   pthread_mutex_lock(&sync->lock);
   for (uint32_t i = 0; i < count; i++) {
      struct kbase_syncobj *obj = obj_get(sync, handles[i]);
      if (obj)
         obj_clear(obj);
      else
         ret = fail(EINVAL);
   }
   pthread_mutex_unlock(&sync->lock);
   return ret;
}

static int
kbase_sync_signal(struct util_sync_provider *p, const uint32_t *handles, uint32_t count)
{
   struct kbase_sync *sync = to_sync(p);
   int ret = 0;

   pthread_mutex_lock(&sync->lock);
   for (uint32_t i = 0; i < count; i++) {
      struct kbase_syncobj *obj = obj_get(sync, handles[i]);
      if (obj)
         obj_replace(obj, NULL);
      else
         ret = fail(EINVAL);
   }
   sync_changed(sync);
   pthread_mutex_unlock(&sync->lock);
   return ret;
}

static int
kbase_sync_timeline_signal(struct util_sync_provider *p, const uint32_t *handles,
                           uint64_t *points, uint32_t count)
{
   struct kbase_sync *sync = to_sync(p);
   int ret = 0;

   pthread_mutex_lock(&sync->lock);
   for (uint32_t i = 0; i < count; i++) {
      struct kbase_syncobj *obj = obj_get(sync, handles[i]);
      if (!obj)
         ret = fail(EINVAL);
      else if (points[i])
         obj_add_point(obj, points[i], NULL);
      else
         obj_replace(obj, NULL);
   }
   sync_changed(sync);
   pthread_mutex_unlock(&sync->lock);
   return ret;
}

static int
kbase_sync_query(struct util_sync_provider *p, uint32_t *handles, uint64_t *points,
                 uint32_t count, uint32_t flags)
{
   struct kbase_sync *sync = to_sync(p);
   int ret = 0;

   pthread_mutex_lock(&sync->lock);
   for (uint32_t i = 0; i < count; i++) {
      struct kbase_syncobj *obj = obj_get(sync, handles[i]);
      if (!obj) {
         ret = fail(EINVAL);
         continue;
      }
      obj_prune(sync, obj);
      if (flags & DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED)
         points[i] = obj->last_point;
      else
         points[i] = obj->signalled_point;
   }
   pthread_mutex_unlock(&sync->lock);
   return ret;
}

int
kbase_sync_attach(struct kbase_sync *sync, uint32_t handle, uint64_t point,
                  struct kbase_fence *fence)
{
   int ret = 0;

   pthread_mutex_lock(&sync->lock);
   struct kbase_syncobj *obj = obj_get(sync, handle);
   if (!obj)
      ret = fail(EINVAL);
   else if (point)
      obj_add_point(obj, point, fence_ref(fence));
   else
      obj_replace(obj, fence_ref(fence));
   sync_changed(sync);
   pthread_mutex_unlock(&sync->lock);
   return ret;
}

int
kbase_sync_get_fence(struct kbase_sync *sync, uint32_t handle, uint64_t point,
                     struct kbase_fence **out)
{
   int err;

   pthread_mutex_lock(&sync->lock);
   struct kbase_syncobj *obj = obj_get(sync, handle);
   bool available = obj && obj_fence_for(sync, obj, point, out, &err);
   pthread_mutex_unlock(&sync->lock);

   if (!obj)
      return fail(EINVAL);
   if (!available)
      return fail(EINVAL);
   return err ? fail(-err) : 0;
}

const struct kbase_fence_cond *
kbase_fence_conds(const struct kbase_fence *f, uint32_t *count)
{
   *count = f ? f->count : 0;
   return f ? f->conds : NULL;
}

/* Reads every pending notification; marks groups with fatal errors and tells the fatal hook.
 * Called without the lock. Notifications read by any thread but the polling waiter wake that
 * waiter ([wake]): kbase's wake-up is consumed by the read. */
static void
drain(struct kbase_sync *sync, bool wake)
{
   unsigned got_any = 0;
   for (unsigned guard = 0; guard < 64; guard++) {
      struct pollfd pfd = { .fd = sync->fd, .events = POLLIN };
      if (poll(&pfd, 1, 0) <= 0 || !(pfd.revents & POLLIN))
         break;

      struct base_csf_notification note;
      ssize_t got = read(sync->fd, &note, sizeof(note));
      if (got != sizeof(note))
         break;
      got_any++;

      if (note.type == BASE_CSF_NOTIFICATION_GPU_QUEUE_GROUP_ERROR) {
         uint8_t group = note.payload.csg_error.handle;
         const struct base_gpu_queue_group_error *err = &note.payload.csg_error.error;
         /* A queue fault (CS_FAULT reporting) is not fatal on its own; everything else is. */
         if (err->error_type != BASE_GPU_QUEUE_GROUP_QUEUE_ERROR_FAULT &&
             !__atomic_exchange_n(&sync->group_fatal[group], true, __ATOMIC_ACQ_REL)) {
            void (*hook)(void *, uint8_t) = __atomic_load_n(&sync->fatal_hook, __ATOMIC_ACQUIRE);
            if (hook)
               hook(__atomic_load_n(&sync->fatal_data, __ATOMIC_ACQUIRE), group);
         }
         /* status: the exception type in the low byte (kbase uAPI csf/mali_base_csf_kernel.h). */
         if (err->error_type == BASE_GPU_QUEUE_GROUP_ERROR_FATAL ||
             err->error_type == BASE_GPU_QUEUE_GROUP_ERROR_TIMEOUT ||
             err->error_type == BASE_GPU_QUEUE_GROUP_ERROR_TILER_HEAP_OOM) {
            mesa_loge("kbase: queue group %u error type %u: status 0x%x sideband 0x%" PRIx64,
                      group, err->error_type, err->payload.fatal_group.status,
                      (uint64_t)err->payload.fatal_group.sideband);
         } else {
            mesa_loge("kbase: queue group %u error type %u: queue %u status 0x%x sideband 0x%"
                      PRIx64, group, err->error_type, err->payload.fatal_queue.csi_index,
                      err->payload.fatal_queue.status,
                      (uint64_t)err->payload.fatal_queue.sideband);
         }
      }
   }
   if (wake && got_any)
      sync_wake(sync);
}

void
kbase_sync_drain_notifications(struct kbase_sync *sync)
{
   drain(sync, true);
}

bool
kbase_sync_group_fatal(const struct kbase_sync *sync, uint8_t group)
{
   return __atomic_load_n(&sync->group_fatal[group], __ATOMIC_ACQUIRE);
}

void
kbase_sync_group_reset(struct kbase_sync *sync, uint8_t group)
{
   __atomic_store_n(&sync->group_fatal[group], false, __ATOMIC_RELEASE);
}

void
kbase_sync_set_fatal_hook(struct kbase_sync *sync, void (*hook)(void *data, uint8_t group),
                          void *data)
{
   __atomic_store_n(&sync->fatal_data, data, __ATOMIC_RELEASE);
   __atomic_store_n(&sync->fatal_hook, hook, __ATOMIC_RELEASE);
}

static int
kbase_sync_timeline_wait(struct util_sync_provider *p, uint32_t *handles, uint64_t *points,
                         unsigned count, int64_t abs_timeout_ns, unsigned flags,
                         uint32_t *first_signaled)
{
   struct kbase_sync *sync = to_sync(p);
   const bool wait_all = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL;
   const bool for_submit = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT;
   const bool available_only = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE;

   if (!count)
      return 0;

   pthread_mutex_lock(&sync->lock);
   for (;;) {
      unsigned done = 0;
      int first = -1;
      bool need_gpu = false;

      for (unsigned i = 0; i < count; i++) {
         struct kbase_syncobj *obj = obj_get(sync, handles[i]);
         if (!obj) {
            pthread_mutex_unlock(&sync->lock);
            return fail(EINVAL);
         }

         struct kbase_fence *f;
         int err;
         if (!obj_fence_for(sync, obj, points ? points[i] : 0, &f, &err)) {
            if (!for_submit) {
               pthread_mutex_unlock(&sync->lock);
               return fail(EINVAL);
            }
            continue;
         }
         if (err) {
            pthread_mutex_unlock(&sync->lock);
            return fail(-err);
         }

         if (available_only || !f) {
            done++;
            if (first < 0)
               first = i;
         } else {
            need_gpu = true;
         }
         fence_unref(f);
      }

      if ((wait_all && done == count) || (!wait_all && done > 0)) {
         pthread_mutex_unlock(&sync->lock);
         if (first_signaled)
            *first_signaled = first;
         return 0;
      }

      int64_t now = os_time_get_nano();
      if (now >= abs_timeout_ns) {
         pthread_mutex_unlock(&sync->lock);
         return fail(ETIME);
      }

      int64_t slice_end =
         MIN2(abs_timeout_ns, now + (int64_t)KBASE_SYNC_POLL_SLICE_MS * 1000000);

      if (need_gpu && !sync->pumping) {
         /* Become the reader: sleep in poll() until kbase reports an event or error, or the
          * eventfd says something changed (a payload, or notifications another thread read). */
         sync->pumping = true;
         pthread_mutex_unlock(&sync->lock);

         struct pollfd pfd[2] = {
            { .fd = sync->fd, .events = POLLIN },
            { .fd = sync->wake_fd, .events = POLLIN },
         };
         int ms = DIV_ROUND_UP(slice_end - now, 1000000);
         if (poll(pfd, sync->wake_fd >= 0 ? 2 : 1, ms) > 0) {
            if (pfd[1].revents & POLLIN) {
               uint64_t count;
               ssize_t ret = read(sync->wake_fd, &count, sizeof(count));
               (void)ret;
            }
            if (pfd[0].revents & POLLIN)
               drain(sync, false);
         }

         pthread_mutex_lock(&sync->lock);
         sync->pumping = false;
         pthread_cond_broadcast(&sync->changed);
      } else {
         /* Waiting for a submission, a CPU signal, or the reader's next round. */
         struct timespec ts = {
            .tv_sec = slice_end / 1000000000,
            .tv_nsec = slice_end % 1000000000,
         };
         pthread_cond_timedwait(&sync->changed, &sync->lock, &ts);
      }
   }
}

static int
kbase_sync_wait(struct util_sync_provider *p, uint32_t *handles, unsigned count,
                int64_t abs_timeout_ns, unsigned flags, uint32_t *first_signaled)
{
   return kbase_sync_timeline_wait(p, handles, NULL, count, abs_timeout_ns, flags,
                                   first_signaled);
}

static int
kbase_sync_transfer(struct util_sync_provider *p, uint32_t dst_handle, uint64_t dst_point,
                    uint32_t src_handle, uint64_t src_point, uint32_t flags)
{
   struct kbase_sync *sync = to_sync(p);

   if (flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT) {
      int ret = kbase_sync_timeline_wait(p, &src_handle, &src_point, 1, INT64_MAX,
                                         DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT |
                                            DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE,
                                         NULL);
      if (ret)
         return ret;
   }

   int ret = 0;
   pthread_mutex_lock(&sync->lock);
   struct kbase_syncobj *src = obj_get(sync, src_handle);
   struct kbase_syncobj *dst = obj_get(sync, dst_handle);
   struct kbase_fence *f = NULL;
   int err;
   if (!src || !dst || !obj_fence_for(sync, src, src_point, &f, &err)) {
      ret = fail(EINVAL);
   } else if (err) {
      ret = fail(-err);
   } else {
      if (dst_point)
         obj_add_point(dst, dst_point, f);
      else
         obj_replace(dst, f);
      f = NULL;
      sync_changed(sync);
   }
   pthread_mutex_unlock(&sync->lock);
   fence_unref(f);
   return ret;
}

static void
kbase_sync_unref(struct kbase_sync *sync)
{
   if (!p_atomic_dec_zero(&sync->refcnt))
      return;

   util_dynarray_foreach(&sync->objs, struct kbase_syncobj *, objp) {
      if (*objp) {
         obj_clear(*objp);
         util_dynarray_fini(&(*objp)->nodes);
         free(*objp);
      }
   }
   util_dynarray_fini(&sync->objs);
   if (sync->wake_fd >= 0)
      close(sync->wake_fd);
   pthread_cond_destroy(&sync->changed);
   pthread_mutex_destroy(&sync->lock);
   free(sync);
}

static void
kbase_sync_finalize(struct util_sync_provider *p)
{
   kbase_sync_unref(to_sync(p));
   free(p);
}

static struct util_sync_provider *kbase_sync_provider_wrap(struct kbase_sync *sync);

static struct util_sync_provider *
kbase_sync_clone(struct util_sync_provider *p)
{
   struct kbase_sync *sync = to_sync(p);
   p_atomic_inc(&sync->refcnt);
   return kbase_sync_provider_wrap(sync);
}

static struct util_sync_provider *
kbase_sync_provider_wrap(struct kbase_sync *sync)
{
   struct kbase_sync_provider *sp = calloc(1, sizeof(*sp));
   if (!sp) {
      kbase_sync_unref(sync);
      return NULL;
   }

   sp->sync = sync;
   sp->base = (struct util_sync_provider){
      .create = kbase_sync_create,
      .destroy = kbase_sync_destroy,
      .handle_to_fd = kbase_sync_unsupported_fd,
      .fd_to_handle = kbase_sync_unsupported_import,
      .import_sync_file = kbase_sync_unsupported_import_file,
      .export_sync_file = kbase_sync_unsupported_fd,
      .wait = kbase_sync_wait,
      .reset = kbase_sync_reset,
      .signal = kbase_sync_signal,
      .timeline_signal = kbase_sync_timeline_signal,
      .timeline_wait = kbase_sync_timeline_wait,
      .query = kbase_sync_query,
      .transfer = kbase_sync_transfer,
      .finalize = kbase_sync_finalize,
      .clone = kbase_sync_clone,
   };
   return &sp->base;
}

struct util_sync_provider *
kbase_sync_provider_create(struct kbase_kmod_dev *kdev)
{
   struct kbase_sync *sync = calloc(1, sizeof(*sync));
   if (!sync)
      return NULL;

   sync->refcnt = 1;
   sync->fd = kdev->base.fd;
   sync->wake_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
   if (sync->wake_fd < 0)
      mesa_logw("kbase: no eventfd (%s): CPU signals reach GPU waits at the next poll slice",
                strerror(errno));
   pthread_mutex_init(&sync->lock, NULL);

   /* Timeouts are absolute CLOCK_MONOTONIC times, as DRM's. */
   pthread_condattr_t attr;
   pthread_condattr_init(&attr);
   pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
   pthread_cond_init(&sync->changed, &attr);
   pthread_condattr_destroy(&attr);

   util_dynarray_init(&sync->objs, NULL);
   util_dynarray_append(&sync->objs, (struct kbase_syncobj *)NULL); /* handle 0 is invalid */

   kdev->sync = sync;
   return kbase_sync_provider_wrap(sync);
}
