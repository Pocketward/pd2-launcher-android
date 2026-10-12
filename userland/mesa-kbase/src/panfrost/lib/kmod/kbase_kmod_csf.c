/*
 * Queue groups, rings and submission for the kbase backend: the panthor command-stream ioctls
 * PanVK issues (GROUP_CREATE, GROUP_DESTROY, GROUP_SUBMIT, GROUP_GET_STATE, TILER_HEAP_CREATE,
 * TILER_HEAP_DESTROY) carried out on kbase's CSF queues (pan_kmod_ops::ioctl).
 *
 * A panthor group is a kbase queue group (CS_QUEUE_GROUP_CREATE); each of its queues is a ring
 * buffer of our own (CS_QUEUE_REGISTER, CS_QUEUE_BIND, then a mapping of the queue's doorbell,
 * input and output pages). What panthor's kernel writes into its ring for a job, we write: waits
 * on other rings' sync objects, a cache flush, a CALL of PanVK's stream, and a 64-bit sync object
 * increment that marks the job done; then the insert pointer moves and CS_QUEUE_KICK starts the
 * queue. The sync objects live in CSF event memory: kbase only re-evaluates a suspended group's
 * waits through that memory's kernel mapping.
 */

#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "drm-uapi/panthor_drm.h"

#include "util/log.h"
#include "util/macros.h"
#include "util/os_mman.h"
#include "util/os_misc.h"
#include "util/os_time.h"
#include "util/simple_mtx.h"
#include "util/u_dynarray.h"
#include "util/u_math.h"

#include "kbase-uapi/mali_base_kernel.h"
#include "kbase-uapi/mali_kbase_ioctl.h"

/* The CS instructions our ring code uses are encoded alike on v10 to v13 (the later fields
 * default to zero), as panthor's kernel encodes them once for every arch. */
#define PAN_ARCH 10
#include "genxml/gen_macros.h"

#include "kbase_kmod.h"
#include "kbase_kmod_sync.h"
#include "pan_kmod_backend.h"

#define KBASE_CSF_MAX_QUEUES 8
#define KBASE_CSF_MAX_HEAPS 64

/* One 64-bit sync object per 16 bytes (BASEP_EVENT64_ALIGN_BYTES). */
#define KBASE_SYNC_STRIDE 16

/* How long a full ring may stay full before the submit gives up. */
#define KBASE_RING_FULL_TIMEOUT_NS (10ull * 1000000000ull)

struct kbase_queue {
   uint32_t index;
   uint32_t ring_size;
   uint8_t *ring;      /* SAME_VA: CPU address == GPU VA; aligned to ring_size */
   uint8_t *ring_alloc; /* the allocation holding it, twice the size */
   uint8_t *io;        /* doorbell, input and output pages */
   volatile uint64_t *insert;
   uint64_t insert_off; /* bytes written so far */
   uint64_t *sync;     /* our sync object in the group's event page */
   uint64_t seqno;     /* the value the last job's increment reaches */
   uint64_t committed_seqno; /* seqno as of the last commit (staging rolls back to it) */
   uint32_t full_waits; /* commits that found the ring full (diagnostics) */
   struct util_dynarray staged; /* uint64_t instructions not yet in the ring */
   bool staged_open;   /* instructions staged after the last increment */

   /* Ring space: every committed batch ends with an increment of our sync object. The increment
    * signals before the command stream has read the rest of its batch (the error barrier and the
    * padding after it), so a batch's ring space is free only once a later batch's increment is
    * seen. A FIFO of (value, end) per batch in flight; at most one batch per 64 bytes of ring. */
   struct {
      uint64_t value;
      uint64_t end;
   } *batches;
   uint32_t batch_cap, batch_head, batch_count;
   uint64_t retired_off; /* the ring is free up to here */
};

struct kbase_group {
   uint8_t handle;
   uint32_t queue_count;
   struct kbase_queue queues[KBASE_CSF_MAX_QUEUES];
   uint64_t *sync_page;
   simple_mtx_t lock;
};

struct kbase_csf {
   simple_mtx_t lock;
   struct kbase_group *groups[256];
   uint64_t heaps[KBASE_CSF_MAX_HEAPS]; /* tiler heap context VAs; 0: free */
};

static struct kbase_csf *
csf_state(struct kbase_kmod_dev *kdev)
{
   if (!kdev->csf) {
      struct kbase_csf *csf = calloc(1, sizeof(*csf));
      if (!csf)
         return NULL;
      simple_mtx_init(&csf->lock, mtx_plain);
      if (p_atomic_cmpxchg(&kdev->csf, NULL, csf) != NULL) {
         simple_mtx_destroy(&csf->lock);
         free(csf);
      }
   }
   return kdev->csf;
}

/* --- SAME_VA allocations for our own rings and sync objects ------------------------------- */

static void *
same_va_alloc(struct kbase_kmod_dev *kdev, uint64_t size, uint64_t flags)
{
   uint64_t pages = DIV_ROUND_UP(size, kdev->page_size);
   union kbase_ioctl_mem_alloc req = {
      .in = { .va_pages = pages, .commit_pages = pages, .flags = flags },
   };

   if (kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_MEM_ALLOC, &req)) {
      mesa_loge("kbase: MEM_ALLOC of %" PRIu64 " pages (flags 0x%" PRIx64 ") failed: %s", pages,
                (uint64_t)flags, strerror(errno));
      return NULL;
   }

   /* The returned "address" is a cookie; mapping it gives the region its GPU VA, the CPU one. */
   void *cpu = os_mmap(NULL, pages * kdev->page_size, PROT_READ | PROT_WRITE, MAP_SHARED,
                       kdev->base.fd, req.out.gpu_va);
   if (cpu == MAP_FAILED) {
      mesa_loge("kbase: mapping a SAME_VA allocation failed: %s", strerror(errno));
      struct kbase_ioctl_mem_free free_req = { .gpu_addr = req.out.gpu_va };
      kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_MEM_FREE, &free_req);
      return NULL;
   }

   return cpu;
}

static void
same_va_free(struct kbase_kmod_dev *kdev, void *cpu, uint64_t size)
{
   /* Unmapping a SAME_VA region frees it. */
   if (cpu)
      os_munmap(cpu, align64(size, kdev->page_size));
}

/* --- groups ------------------------------------------------------------------------------- */

static uint8_t
to_kbase_priority(uint8_t panthor_priority)
{
   switch (panthor_priority) {
   case PANTHOR_GROUP_PRIORITY_REALTIME:
      return BASE_QUEUE_GROUP_PRIORITY_REALTIME;
   case PANTHOR_GROUP_PRIORITY_HIGH:
      return BASE_QUEUE_GROUP_PRIORITY_HIGH;
   case PANTHOR_GROUP_PRIORITY_LOW:
      return BASE_QUEUE_GROUP_PRIORITY_LOW;
   default:
      return BASE_QUEUE_GROUP_PRIORITY_MEDIUM;
   }
}

/* CS_QUEUE_GROUP_CREATE in the layout of the negotiated uAPI version. */
static int
kbase_group_create_ioctl(struct kbase_kmod_dev *kdev, const struct drm_panthor_group_create *gc,
                         uint32_t queue_count, uint8_t *handle)
{
   int fd = kdev->base.fd;
   uint8_t prio = to_kbase_priority(gc->priority);
   int ret;

#define FILL_COMMON(in)                                                                        \
   do {                                                                                        \
      (in).tiler_mask = gc->tiler_core_mask;                                                   \
      (in).fragment_mask = gc->fragment_core_mask;                                             \
      (in).compute_mask = gc->compute_core_mask;                                               \
      (in).cs_min = queue_count;                                                               \
      (in).priority = prio;                                                                    \
      (in).tiler_max = gc->max_tiler_cores;                                                    \
      (in).fragment_max = gc->max_fragment_cores;                                              \
      (in).compute_max = gc->max_compute_cores;                                                \
      (in).csi_handlers = BASE_CSF_TILER_OOM_EXCEPTION_FLAG;                                   \
   } while (0)

   if (kbase_kmod_uapi_at_least(kdev, 36)) {
      union kbase_ioctl_cs_queue_group_create req = { 0 };
      FILL_COMMON(req.in);
      ret = kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_QUEUE_GROUP_CREATE, &req);
      *handle = req.out.group_handle;
   } else if (kbase_kmod_uapi_at_least(kdev, 19)) {
      union kbase_ioctl_cs_queue_group_create_1_35 req = { 0 };
      FILL_COMMON(req.in);
      ret = kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_QUEUE_GROUP_CREATE_1_35, &req);
      *handle = req.out.group_handle;
   } else {
      union kbase_ioctl_cs_queue_group_create_1_18 req = { 0 };
      FILL_COMMON(req.in);
      ret = kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_QUEUE_GROUP_CREATE_1_18, &req);
      *handle = req.out.group_handle;
   }
#undef FILL_COMMON

   if (ret)
      mesa_loge("kbase: CS_QUEUE_GROUP_CREATE failed: %s", strerror(errno));
   return ret;
}

static void
kbase_queue_fini(struct kbase_kmod_dev *kdev, struct kbase_queue *q, bool registered)
{
   if (q->full_waits)
      mesa_logi("kbase: queue %u: %u submissions waited for ring space, %" PRIu64
                " bytes written to its %u-byte ring", q->index, q->full_waits, q->insert_off,
                q->ring_size);

   if (registered && q->ring) {
      struct kbase_ioctl_cs_queue_terminate term = { .buffer_gpu_addr = (uintptr_t)q->ring };
      if (kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_QUEUE_TERMINATE, &term))
         mesa_loge("kbase: CS_QUEUE_TERMINATE failed: %s", strerror(errno));
   }
   if (q->io)
      os_munmap(q->io, 3 * kdev->page_size);
   same_va_free(kdev, q->ring_alloc, 2 * (uint64_t)q->ring_size);
   q->ring_alloc = NULL;
   util_dynarray_fini(&q->staged);
   free(q->batches);
   q->batches = NULL;
   q->io = NULL;
   q->ring = NULL;
}

static void
kbase_group_free(struct kbase_kmod_dev *kdev, struct kbase_group *g, bool created)
{
   if (created) {
      struct kbase_ioctl_cs_queue_group_term term = { .group_handle = g->handle };
      if (kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_QUEUE_GROUP_TERMINATE, &term))
         mesa_loge("kbase: CS_QUEUE_GROUP_TERMINATE failed: %s", strerror(errno));
   }
   for (uint32_t i = 0; i < g->queue_count; i++)
      kbase_queue_fini(kdev, &g->queues[i], true);
   same_va_free(kdev, g->sync_page, kdev->page_size);
   simple_mtx_destroy(&g->lock);
   free(g);
}

static int
kbase_queue_init(struct kbase_kmod_dev *kdev, struct kbase_group *g, uint32_t index,
                 const struct drm_panthor_queue_create *qc)
{
   struct kbase_queue *q = &g->queues[index];
   int fd = kdev->base.fd;

   util_dynarray_init(&q->staged, NULL);
   q->index = index;
   q->ring_size = MAX2(util_next_power_of_two(qc->ringbuf_size), kdev->page_size);

   /* CPU-written, read by the command stream front end through no GPU cache. The ring's address
    * is a multiple of its size: a ring at another address can fault (CS_CONFIG_FAULT) once the
    * queue has used some of it, so kbase SAME_VA memory, which is only page aligned, is
    * allocated at twice the size and the aligned half registered (CS_QUEUE_REGISTER accepts part
    * of a region). */
   q->ring_alloc = same_va_alloc(kdev, 2 * (uint64_t)q->ring_size,
                                 BASE_MEM_PROT_CPU_RD | BASE_MEM_PROT_CPU_WR |
                                    BASE_MEM_PROT_GPU_RD | BASE_MEM_UNCACHED_GPU);
   if (!q->ring_alloc)
      return -1;
   q->ring = (uint8_t *)(uintptr_t)align64((uintptr_t)q->ring_alloc, q->ring_size);
   memset(q->ring, 0, q->ring_size);

   struct kbase_ioctl_cs_queue_register reg = {
      .buffer_gpu_addr = (uintptr_t)q->ring,
      .buffer_size = q->ring_size,
      .priority = MIN2(qc->priority, BASE_QUEUE_MAX_PRIORITY),
   };
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_QUEUE_REGISTER, &reg)) {
      mesa_loge("kbase: CS_QUEUE_REGISTER failed: %s", strerror(errno));
      same_va_free(kdev, q->ring_alloc, 2 * (uint64_t)q->ring_size);
      q->ring_alloc = NULL;
      q->ring = NULL;
      return -1;
   }

   union kbase_ioctl_cs_queue_bind bind = {
      .in = {
         .buffer_gpu_addr = (uintptr_t)q->ring,
         .group_handle = g->handle,
         .csi_index = index,
      },
   };
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_QUEUE_BIND, &bind)) {
      mesa_loge("kbase: CS_QUEUE_BIND failed: %s", strerror(errno));
      return -1;
   }

   /* Doorbell, input and output pages (kbase_csf_cpu_mmap_user_io_pages). From uAPI 1.35 the
    * input and output pages are the group's, each queue's registers 16 bytes apart. */
   q->io = os_mmap(NULL, 3 * kdev->page_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                   bind.out.mmap_handle);
   if (q->io == MAP_FAILED) {
      mesa_loge("kbase: mapping the queue's IO pages failed: %s", strerror(errno));
      q->io = NULL;
      return -1;
   }

   size_t off = kbase_kmod_uapi_at_least(kdev, 35) ? index * 16 : 0;
   q->insert = (volatile uint64_t *)(q->io + kdev->page_size + off);
   q->insert_off = *q->insert;
   q->retired_off = q->insert_off;

   /* We do not read CS_EXTRACT (the output page): kbase need not keep it in step with the
    * firmware's progress, and writing on that basis can overrun unread ring contents. */
   q->batch_cap = q->ring_size / 64 + 1;
   q->batches = calloc(q->batch_cap, sizeof(*q->batches));
   if (!q->batches)
      return -1;

   q->sync = (uint64_t *)((uint8_t *)g->sync_page + index * KBASE_SYNC_STRIDE);
   q->seqno = __atomic_load_n(q->sync, __ATOMIC_ACQUIRE);
   q->committed_seqno = q->seqno;
   return 0;
}

static int
kbase_csf_group_create(struct kbase_kmod_dev *kdev, struct drm_panthor_group_create *gc)
{
   struct kbase_csf *csf = csf_state(kdev);
   uint32_t count = gc->queues.count;

   if (!csf || !count || count > KBASE_CSF_MAX_QUEUES || count > kdev->csif.cs_slot_count ||
       gc->queues.stride < sizeof(struct drm_panthor_queue_create)) {
      errno = EINVAL;
      return -1;
   }

   struct kbase_group *g = calloc(1, sizeof(*g));
   if (!g) {
      errno = ENOMEM;
      return -1;
   }
   simple_mtx_init(&g->lock, mtx_plain);

   g->sync_page = same_va_alloc(kdev, kdev->page_size,
                                BASE_MEM_CSF_EVENT | BASE_MEM_PROT_CPU_RD |
                                   BASE_MEM_PROT_CPU_WR | BASE_MEM_PROT_GPU_RD |
                                   BASE_MEM_PROT_GPU_WR);
   if (!g->sync_page)
      goto err_free;
   memset(g->sync_page, 0, kdev->page_size);

   if (kbase_group_create_ioctl(kdev, gc, count, &g->handle))
      goto err_free;

   const uint8_t *qcs = (const uint8_t *)(uintptr_t)gc->queues.array;
   for (uint32_t i = 0; i < count; i++) {
      g->queue_count = i + 1;
      if (kbase_queue_init(kdev, g, i,
                           (const struct drm_panthor_queue_create *)(qcs + i * gc->queues.stride)))
         goto err_destroy;
   }

   kbase_sync_group_reset(kdev->sync, g->handle);
   simple_mtx_lock(&csf->lock);
   csf->groups[g->handle] = g;
   simple_mtx_unlock(&csf->lock);

   gc->group_handle = g->handle;
   return 0;

err_destroy:
   kbase_group_free(kdev, g, true);
   errno = EINVAL;
   return -1;
err_free:
   same_va_free(kdev, g->sync_page, kdev->page_size);
   simple_mtx_destroy(&g->lock);
   free(g);
   errno = ENOMEM;
   return -1;
}

static struct kbase_group *
kbase_group_get(struct kbase_kmod_dev *kdev, uint32_t handle)
{
   struct kbase_csf *csf = csf_state(kdev);
   if (!csf || handle >= ARRAY_SIZE(csf->groups))
      return NULL;

   simple_mtx_lock(&csf->lock);
   struct kbase_group *g = csf->groups[handle];
   simple_mtx_unlock(&csf->lock);
   return g;
}

static int
kbase_csf_group_destroy(struct kbase_kmod_dev *kdev, struct drm_panthor_group_destroy *gd)
{
   struct kbase_csf *csf = csf_state(kdev);
   struct kbase_group *g = kbase_group_get(kdev, gd->group_handle);
   if (!g) {
      errno = EINVAL;
      return -1;
   }

   simple_mtx_lock(&csf->lock);
   csf->groups[g->handle] = NULL;
   simple_mtx_unlock(&csf->lock);

   kbase_group_free(kdev, g, true);
   return 0;
}

static int
kbase_csf_group_get_state(struct kbase_kmod_dev *kdev, struct drm_panthor_group_get_state *gs)
{
   struct kbase_group *g = kbase_group_get(kdev, gs->group_handle);
   if (!g) {
      errno = EINVAL;
      return -1;
   }

   kbase_sync_drain_notifications(kdev->sync);
   bool fatal = kbase_sync_group_fatal(kdev->sync, g->handle);
   gs->state = fatal ? DRM_PANTHOR_GROUP_STATE_FATAL_FAULT : 0;
   gs->fatal_queues = fatal ? BITFIELD_MASK(g->queue_count) : 0;
   return 0;
}

/* --- submission --------------------------------------------------------------------------- */

/* All earlier stores complete (reach memory or the device) before any later one. */
static inline void
device_write_barrier(void)
{
#if defined(__aarch64__)
   __asm__ volatile("dsb st" ::: "memory");
#else
   __atomic_thread_fence(__ATOMIC_SEQ_CST);
#endif
}

static uint64_t *
stage(struct kbase_queue *q)
{
   q->staged_open = true;
   return util_dynarray_grow(&q->staged, uint64_t, 1);
}

static void
emit_move48(struct kbase_queue *q, unsigned reg, uint64_t value)
{
   pan_cast_and_pack(stage(q), CS_MOVE48, I) {
      I.destination = reg;
      I.immediate = value;
   }
}

static void
emit_move32(struct kbase_queue *q, unsigned reg, uint32_t value)
{
   pan_cast_and_pack(stage(q), CS_MOVE32, I) {
      I.destination = reg;
      I.immediate = value;
   }
}

static void
emit_wait(struct kbase_queue *q, uint32_t mask)
{
   pan_cast_and_pack(stage(q), CS_WAIT, I) {
      I.wait_mask = mask;
   }
}

/* Registers of the top KBASE_KMOD_RING_REG_COUNT, which PanVK's streams leave to us. */
struct ring_regs {
   unsigned addr; /* 64-bit pair */
   unsigned val;  /* 64-bit pair */
};

static void
emit_gpu_wait(struct kbase_queue *q, const struct ring_regs *r, uint64_t va, uint64_t value)
{
   /* Wait until *va > value - 1. */
   emit_move48(q, r->addr, va);
   emit_move48(q, r->val, value - 1);
   pan_cast_and_pack(stage(q), CS_SYNC_WAIT64, I) {
      I.error_reject = false;
      I.condition = MALI_CS_CONDITION_GREATER;
      I.data = r->val;
      I.address = r->addr;
   }
}

static void
emit_call(struct kbase_queue *q, const struct ring_regs *r, uint64_t addr, uint32_t size,
          uint32_t latest_flush)
{
   /* Clean and invalidate the caches the stream will read through, unless nothing was flushed
    * since latest_flush (the flush ID check), then call the stream. */
   emit_move32(q, r->val, latest_flush);
   pan_cast_and_pack(stage(q), CS_FLUSH_CACHE2, I) {
      I.l2_flush_mode = MALI_CS_FLUSH_MODE_CLEAN_AND_INVALIDATE;
      I.lsc_flush_mode = MALI_CS_FLUSH_MODE_CLEAN_AND_INVALIDATE;
      I.other_flush_mode = MALI_CS_OTHER_FLUSH_MODE_INVALIDATE;
      I.latest_flush_id = r->val;
      I.signal_slot = 0;
      I.wait_mask = 0;
   }
   emit_move48(q, r->addr, addr);
   emit_move32(q, r->val, size);
   emit_wait(q, BITFIELD_BIT(0));
   pan_cast_and_pack(stage(q), CS_CALL, I) {
      I.address = r->addr;
      I.length = r->val;
   }
}

static void
emit_done(struct kbase_queue *q, const struct ring_regs *r, uint32_t sb_count)
{
   /* Everything before is done: add one to our sync object, system-wide visible, carrying an
    * error from the stream into it; then stop at an error barrier. */
   emit_move48(q, r->addr, (uintptr_t)q->sync);
   emit_move48(q, r->val, 1);
   emit_wait(q, BITFIELD_MASK(sb_count));
   pan_cast_and_pack(stage(q), CS_SYNC_ADD64, I) {
      I.error_propagate = true;
      I.scope = MALI_CS_SYNC_SCOPE_SYSTEM;
      I.wait_mask = 0;
      I.data = r->val;
      I.address = r->addr;
      I.signal_slot = 0;
   }
   pan_cast_and_pack(stage(q), CS_ERROR_BARRIER, I);
   q->seqno++;
   q->staged_open = false;
}

/* Logs every queue's ring and sync state (when a ring stays full). */
static void
kbase_group_dump(struct kbase_kmod_dev *kdev, struct kbase_group *g)
{
   size_t stride = kbase_kmod_uapi_at_least(kdev, 35) ? 16 : 0;
   for (uint32_t i = 0; i < g->queue_count; i++) {
      struct kbase_queue *q = &g->queues[i];
      const volatile uint64_t *out =
         (const volatile uint64_t *)(q->io + 2 * kdev->page_size + i * stride);
      mesa_logw("kbase: group %u queue %u: insert 0x%" PRIx64 " (page 0x%" PRIx64 "), free from 0x%"
                PRIx64 ", sync %" PRIu64 " of %" PRIu64 ", CS_EXTRACT 0x%" PRIx64 ", CS_ACTIVE %u",
                g->handle, i, q->insert_off, (uint64_t)*q->insert, q->retired_off,
                __atomic_load_n(q->sync, __ATOMIC_ACQUIRE), q->seqno, (uint64_t)out[0],
                (unsigned)(out[1] & 0xffffffffu));
   }
}

/* Ring space kept free: with less free, queues can fault (CS_CONFIG_FAULT) when their rings fill
 * under load. */
static uint64_t
kbase_ring_headroom(const struct kbase_queue *q)
{
   return MAX2(16 * 1024, q->ring_size / 4);
}

/* Drops what is staged, and the increments it would have made. */
static void
kbase_queue_discard(struct kbase_queue *q)
{
   util_dynarray_clear(&q->staged);
   q->staged_open = false;
   q->seqno = q->committed_seqno;
}

/* Moves retired_off past every batch the command stream has left: those with a later
 * increment already made. */
static void
kbase_queue_retire(struct kbase_queue *q)
{
   uint64_t done = __atomic_load_n(q->sync, __ATOMIC_ACQUIRE);

   while (q->batch_count && q->batches[q->batch_head].value < done) {
      q->retired_off = q->batches[q->batch_head].end;
      q->batch_head = (q->batch_head + 1) % q->batch_cap;
      q->batch_count--;
   }
}

/* Copies the staged instructions into the ring (whole cache lines, as the prefetcher wants), moves
 * the insert pointer and kicks the queue. */
static int
kbase_queue_commit(struct kbase_kmod_dev *kdev, struct kbase_group *g, struct kbase_queue *q,
                   const struct ring_regs *regs)
{
   unsigned count = util_dynarray_num_elements(&q->staged, uint64_t);
   if (!count)
      return 0;

   /* Every batch ends with an increment, so its ring space can be reclaimed. */
   if (q->staged_open)
      emit_done(q, regs, kdev->csif.scoreboard_slot_count);
   count = util_dynarray_num_elements(&q->staged, uint64_t);

   unsigned padded = align(count, 8);
   while (util_dynarray_num_elements(&q->staged, uint64_t) < padded)
      util_dynarray_append(&q->staged, (uint64_t)0); /* NOP */

   uint64_t bytes = padded * sizeof(uint64_t);
   /* The newest batch keeps its space until a later one runs, and the headroom stays free. */
   if (bytes > (q->ring_size - kbase_ring_headroom(q)) / 2) {
      mesa_loge("kbase: queue %u: a %" PRIu64 "-byte batch does not fit its %u-byte ring",
                q->index, bytes, q->ring_size);
      kbase_queue_discard(q);
      errno = E2BIG;
      return -1;
   }

   int64_t deadline = os_time_get_nano() + KBASE_RING_FULL_TIMEOUT_NS;
   bool waited = false;
   int64_t next_report = os_time_get_nano() + 1000000000ll;
   for (;;) {
      kbase_queue_retire(q);
      if (q->insert_off + bytes - q->retired_off + kbase_ring_headroom(q) <= q->ring_size &&
          q->batch_count < q->batch_cap)
         break;

      kbase_sync_drain_notifications(kdev->sync);
      int64_t now = os_time_get_nano();
      if (kbase_sync_group_fatal(kdev->sync, g->handle) || now > deadline) {
         mesa_loge("kbase: queue %u: no ring space (insert 0x%" PRIx64 ", free from 0x%" PRIx64
                   ", sync %" PRIu64 " of %" PRIu64 ")", q->index, q->insert_off,
                   q->retired_off, __atomic_load_n(q->sync, __ATOMIC_ACQUIRE), q->seqno);
         kbase_queue_discard(q);
         errno = EIO;
         return -1;
      }
      if (now > next_report) {
         mesa_logw("kbase: queue %u: waiting for ring space", q->index);
         kbase_group_dump(kdev, g);
         next_report = now + 1000000000ll;
      }
      waited = true;
      usleep(100);
   }
   q->full_waits += waited;

   const uint8_t *src = util_dynarray_begin(&q->staged);
   uint64_t start = q->insert_off & (q->ring_size - 1);
   uint64_t first = MIN2(bytes, q->ring_size - start);
   memcpy(q->ring + start, src, first);
   if (first < bytes)
      memcpy(q->ring, src + first, bytes - first);

   q->insert_off += bytes;
   util_dynarray_clear(&q->staged);
   q->staged_open = false;

   uint32_t tail = (q->batch_head + q->batch_count) % q->batch_cap;
   q->batches[tail].value = q->seqno;
   q->batches[tail].end = q->insert_off;
   q->batch_count++;
   q->committed_seqno = q->seqno;

   /* The ring and the input page are write-combined, and the GPU is not in the CPU's inner
    * shareable domain, so a DMB ISH (what a C11 fence gives) does not order what it observes:
    * make the ring's contents reach memory before the insert pointer moves, and the insert
    * pointer before the kick. */
   device_write_barrier();
   __atomic_store_n(q->insert, q->insert_off, __ATOMIC_RELAXED);
   device_write_barrier();

   struct kbase_ioctl_cs_queue_kick kick = { .buffer_gpu_addr = (uintptr_t)q->ring };
   if (kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_QUEUE_KICK, &kick)) {
      mesa_loge("kbase: CS_QUEUE_KICK failed: %s", strerror(errno));
      return -1;
   }
   return 0;
}

struct pending_signal {
   uint32_t handle;
   uint64_t point;
   struct kbase_queue *queue;
   uint64_t seqno;
};

static int
kbase_csf_group_submit(struct kbase_kmod_dev *kdev, struct drm_panthor_group_submit *gs)
{
   struct kbase_group *g = kbase_group_get(kdev, gs->group_handle);
   if (!g) {
      errno = EINVAL;
      return -1;
   }
   if (kbase_sync_group_fatal(kdev->sync, g->handle)) {
      mesa_loge("kbase: submit to group %u after its fatal error", g->handle);
      errno = EIO;
      return -1;
   }
   if (gs->queue_submits.stride < sizeof(struct drm_panthor_queue_submit)) {
      errno = EINVAL;
      return -1;
   }

   const struct ring_regs regs = {
      .addr = kdev->csif.cs_reg_count - KBASE_KMOD_RING_REG_COUNT,
      .val = kdev->csif.cs_reg_count - KBASE_KMOD_RING_REG_COUNT + 2,
   };
   struct util_dynarray signals;
   util_dynarray_init(&signals, NULL);
   int ret = 0;

   simple_mtx_lock(&g->lock);

   const uint8_t *qss = (const uint8_t *)(uintptr_t)gs->queue_submits.array;
   for (uint32_t i = 0; i < gs->queue_submits.count && !ret; i++) {
      const struct drm_panthor_queue_submit *qs =
         (const void *)(qss + i * gs->queue_submits.stride);
      if (qs->queue_index >= g->queue_count) {
         errno = EINVAL;
         ret = -1;
         break;
      }
      struct kbase_queue *q = &g->queues[qs->queue_index];
      const uint8_t *ops = (const uint8_t *)(uintptr_t)qs->syncs.array;
      bool has_signal = false;

      /* Waits become GPU-side waits on the other rings' sync objects. The Vulkan runtime only
       * submits once every wait has a fence (assisted timeline mode). */
      for (uint32_t s = 0; s < qs->syncs.count && !ret; s++) {
         const struct drm_panthor_sync_op *op = (const void *)(ops + s * qs->syncs.stride);
         if (op->flags & DRM_PANTHOR_SYNC_OP_SIGNAL) {
            has_signal = true;
            continue;
         }

         bool timeline = (op->flags & DRM_PANTHOR_SYNC_OP_HANDLE_TYPE_MASK) ==
                         DRM_PANTHOR_SYNC_OP_HANDLE_TYPE_TIMELINE_SYNCOBJ;
         struct kbase_fence *f;
         if (kbase_sync_get_fence(kdev->sync, op->handle, timeline ? op->timeline_value : 0,
                                  &f)) {
            mesa_loge("kbase: submit waits on syncobj %u point %" PRIu64 " without a fence",
                      op->handle, (uint64_t)op->timeline_value);
            ret = -1;
            break;
         }

         uint32_t n;
         const struct kbase_fence_cond *conds = kbase_fence_conds(f, &n);
         for (uint32_t c = 0; c < n; c++) {
            if (conds[c].value_ptr != q->sync)
               emit_gpu_wait(q, &regs, conds[c].gpu_va, conds[c].value);
         }
         kbase_fence_unref(f);
      }
      if (ret)
         break;

      if (qs->stream_size)
         emit_call(q, &regs, qs->stream_addr, qs->stream_size, qs->latest_flush);

      if (qs->stream_size || has_signal)
         emit_done(q, &regs, kdev->csif.scoreboard_slot_count);

      for (uint32_t s = 0; s < qs->syncs.count; s++) {
         const struct drm_panthor_sync_op *op = (const void *)(ops + s * qs->syncs.stride);
         if (!(op->flags & DRM_PANTHOR_SYNC_OP_SIGNAL))
            continue;
         bool timeline = (op->flags & DRM_PANTHOR_SYNC_OP_HANDLE_TYPE_MASK) ==
                         DRM_PANTHOR_SYNC_OP_HANDLE_TYPE_TIMELINE_SYNCOBJ;
         util_dynarray_append(&signals, ((struct pending_signal){
                                           .handle = op->handle,
                                           .point = timeline ? op->timeline_value : 0,
                                           .queue = q,
                                           .seqno = q->seqno,
                                        }));
      }
   }

   for (uint32_t i = 0; i < g->queue_count; i++) {
      if (ret) {
         /* Nothing staged reaches the ring: forget its increments too. */
         kbase_queue_discard(&g->queues[i]);
      } else if (kbase_queue_commit(kdev, g, &g->queues[i], &regs)) {
         ret = -1;
      }
   }

   simple_mtx_unlock(&g->lock);

   /* Signals take effect once the work is in the rings. */
   if (!ret) {
      util_dynarray_foreach(&signals, struct pending_signal, sig) {
         struct kbase_fence_cond cond = {
            .value_ptr = sig->queue->sync,
            .gpu_va = (uintptr_t)sig->queue->sync,
            .value = sig->seqno,
            .group_handle = g->handle,
         };
         struct kbase_fence *f = kbase_fence_create(&cond, 1);
         if (!f || kbase_sync_attach(kdev->sync, sig->handle, sig->point, f)) {
            mesa_loge("kbase: signalling syncobj %u point %" PRIu64 " failed: %s", sig->handle,
                      sig->point, strerror(errno));
            ret = -1;
         }
         kbase_fence_unref(f);
      }
   }

   util_dynarray_fini(&signals);
   return ret;
}

/* --- tiler heaps -------------------------------------------------------------------------- */

static int
kbase_csf_tiler_heap_create(struct kbase_kmod_dev *kdev, struct drm_panthor_tiler_heap_create *thc)
{
   struct kbase_csf *csf = csf_state(kdev);
   int ret;
   uint64_t ctx_va, first_chunk_va;

   if (!csf) {
      errno = ENOMEM;
      return -1;
   }

   /* kbase keeps target_in_flight in 16 bits. No buffer descriptor: kbase then does not reclaim
    * chunks under memory pressure (csf/mali_kbase_csf_tiler_heap.c). */
   /* PAN_KBASE_HEAP=initial,max,target overrides PanVK's sizes (for debugging). */
   const char *heap_env = os_get_option("PAN_KBASE_HEAP");
   if (heap_env) {
      unsigned initial = thc->initial_chunk_count, max = thc->max_chunks, target = thc->target_in_flight;
      sscanf(heap_env, "%u,%u,%u", &initial, &max, &target);
      thc->initial_chunk_count = initial;
      thc->max_chunks = max;
      thc->target_in_flight = target;
   }
   if (kbase_kmod_uapi_at_least(kdev, 14)) {
      union kbase_ioctl_cs_tiler_heap_init req = {
         .in = {
            .chunk_size = thc->chunk_size,
            .initial_chunks = thc->initial_chunk_count,
            .max_chunks = thc->max_chunks,
            .target_in_flight = MIN2(thc->target_in_flight, UINT16_MAX),
         },
      };
      ret = kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_TILER_HEAP_INIT, &req);
      ctx_va = req.out.gpu_heap_va;
      first_chunk_va = req.out.first_chunk_va;
   } else {
      union kbase_ioctl_cs_tiler_heap_init_1_13 req = {
         .in = {
            .chunk_size = thc->chunk_size,
            .initial_chunks = thc->initial_chunk_count,
            .max_chunks = thc->max_chunks,
            .target_in_flight = MIN2(thc->target_in_flight, UINT16_MAX),
         },
      };
      ret = kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_TILER_HEAP_INIT_1_13, &req);
      ctx_va = req.out.gpu_heap_va;
      first_chunk_va = req.out.first_chunk_va;
   }
   if (ret) {
      mesa_loge("kbase: CS_TILER_HEAP_INIT failed: %s", strerror(errno));
      return -1;
   }

   simple_mtx_lock(&csf->lock);
   uint32_t h;
   for (h = 0; h < KBASE_CSF_MAX_HEAPS && csf->heaps[h]; h++)
      ;
   if (h < KBASE_CSF_MAX_HEAPS)
      csf->heaps[h] = ctx_va;
   simple_mtx_unlock(&csf->lock);

   if (h == KBASE_CSF_MAX_HEAPS) {
      struct kbase_ioctl_cs_tiler_heap_term term = { .gpu_heap_va = ctx_va };
      kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_TILER_HEAP_TERM, &term);
      errno = ENOSPC;
      return -1;
   }

   thc->handle = h;
   thc->tiler_heap_ctx_gpu_va = ctx_va;
   thc->first_heap_chunk_gpu_va = first_chunk_va;
   return 0;
}

static int
kbase_csf_tiler_heap_destroy(struct kbase_kmod_dev *kdev,
                             struct drm_panthor_tiler_heap_destroy *thd)
{
   struct kbase_csf *csf = csf_state(kdev);
   if (!csf || thd->handle >= KBASE_CSF_MAX_HEAPS) {
      errno = EINVAL;
      return -1;
   }

   simple_mtx_lock(&csf->lock);
   uint64_t ctx_va = csf->heaps[thd->handle];
   csf->heaps[thd->handle] = 0;
   simple_mtx_unlock(&csf->lock);

   if (!ctx_va) {
      errno = EINVAL;
      return -1;
   }

   struct kbase_ioctl_cs_tiler_heap_term term = { .gpu_heap_va = ctx_va };
   return kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_TILER_HEAP_TERM, &term);
}

/* --- entry points ------------------------------------------------------------------------- */

int
kbase_kmod_csf_ioctl(struct pan_kmod_dev *dev, unsigned long request, void *arg)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(dev);

   switch (request) {
   case DRM_IOCTL_PANTHOR_GROUP_CREATE:
      return kbase_csf_group_create(kdev, arg);
   case DRM_IOCTL_PANTHOR_GROUP_DESTROY:
      return kbase_csf_group_destroy(kdev, arg);
   case DRM_IOCTL_PANTHOR_GROUP_SUBMIT:
      return kbase_csf_group_submit(kdev, arg);
   case DRM_IOCTL_PANTHOR_GROUP_GET_STATE:
      return kbase_csf_group_get_state(kdev, arg);
   case DRM_IOCTL_PANTHOR_TILER_HEAP_CREATE:
      return kbase_csf_tiler_heap_create(kdev, arg);
   case DRM_IOCTL_PANTHOR_TILER_HEAP_DESTROY:
      return kbase_csf_tiler_heap_destroy(kdev, arg);
   default:
      mesa_loge("kbase: unsupported panthor ioctl 0x%lx", request);
      errno = ENOTTY;
      return -1;
   }
}

void
kbase_csf_dev_finish(struct kbase_kmod_dev *kdev)
{
   struct kbase_csf *csf = kdev->csf;
   if (!csf)
      return;

   for (unsigned i = 0; i < ARRAY_SIZE(csf->groups); i++) {
      if (csf->groups[i])
         kbase_group_free(kdev, csf->groups[i], true);
   }
   for (unsigned h = 0; h < KBASE_CSF_MAX_HEAPS; h++) {
      if (csf->heaps[h]) {
         struct kbase_ioctl_cs_tiler_heap_term term = { .gpu_heap_va = csf->heaps[h] };
         kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_CS_TILER_HEAP_TERM, &term);
      }
   }
   simple_mtx_destroy(&csf->lock);
   free(csf);
   kdev->csf = NULL;
}
