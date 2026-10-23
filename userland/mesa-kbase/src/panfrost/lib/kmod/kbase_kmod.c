/*
 * pan_kmod backend for Arm's mali_kbase kernel driver (/dev/mali0) on CSF GPUs (Mali v10 and
 * later): device open, properties, the GPU VA range, timestamps; memory and the VM.
 */

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <time.h>
#include <unistd.h>

#include "drm-uapi/drm.h"

#include "util/compiler.h"
#include "util/hash_table.h"
#include "util/log.h"
#include "util/macros.h"
#include "util/os_misc.h"
#include "util/os_mman.h"
#include "util/os_time.h"
#include "util/timespec.h"
#include "util/u_atomic.h"
#include "util/u_dynarray.h"
#include "util/u_math.h"
#include "util/u_sync_provider.h"

#include "kbase-uapi/mali_base_kernel.h"
#include "kbase-uapi/mali_kbase_ioctl.h"

#include "kbase_kmod.h"
#include "pan_kmod_backend.h"

/* GPU VA zones of a 64-bit client on CSF (kbase mali_kbase_reg_track.h): the executable zone at
 * 2^47 (4 GiB, kernel-placed), then the executable FIXED zone (4 GiB), then the FIXED zone up to
 * 2^48. We place everything ourselves in the two FIXED zones. */
#define KBASE_EXEC_VA_BASE       (1ull << 47)
#define KBASE_EXEC_FIXED_VA_BASE (KBASE_EXEC_VA_BASE + (1ull << 32))
#define KBASE_FIXED_VA_BASE      (KBASE_EXEC_FIXED_VA_BASE + (1ull << 32))
#define KBASE_FIXED_VA_END       (1ull << 48)

/* GPU VA kbase may use for its own allocations in our context (tiler heap contexts and chunks):
 * PanVK's tiler heaps are at most 64 chunks of 2 MiB per queue. */
#define KBASE_KMOD_CUSTOM_VA_SIZE (4ull << 30)

int
kbase_kmod_ioctl(int fd, unsigned long request, void *arg)
{
   int ret;

   do {
      ret = ioctl(fd, request, arg);
   } while (ret == -1 && (errno == EINTR || errno == EAGAIN));

   return ret;
}

/* GET_GPUPROPS returns (key, value) pairs; the key's low two bits give the value's size. */
struct kbase_props {
   uint64_t value[128];
   bool present[128];
};

static int
kbase_read_props(int fd, struct kbase_props *props)
{
   struct kbase_ioctl_get_gpuprops req = { 0 };
   int size = kbase_kmod_ioctl(fd, KBASE_IOCTL_GET_GPUPROPS, &req);
   if (size <= 0) {
      mesa_loge("kbase: GET_GPUPROPS size query failed (%s)", strerror(errno));
      return -1;
   }

   uint8_t *buf = calloc(1, size);
   if (!buf)
      return -1;

   req.buffer = (uintptr_t)buf;
   req.size = size;
   int got = kbase_kmod_ioctl(fd, KBASE_IOCTL_GET_GPUPROPS, &req);
   if (got <= 0) {
      mesa_loge("kbase: GET_GPUPROPS failed (%s)", strerror(errno));
      free(buf);
      return -1;
   }

   memset(props, 0, sizeof(*props));
   for (int off = 0; off + 4 <= got;) {
      uint32_t key;
      memcpy(&key, buf + off, 4);
      off += 4;

      unsigned bytes = 1u << (key & 3);
      uint32_t id = key >> 2;
      if (off + bytes > (unsigned)got)
         break;

      uint64_t v = 0;
      memcpy(&v, buf + off, bytes);
      off += bytes;

      if (id < ARRAY_SIZE(props->value)) {
         props->value[id] = v;
         props->present[id] = true;
      }
   }

   free(buf);
   return 0;
}

static int
kbase_read_csf_iface(struct kbase_kmod_dev *kdev)
{
   int fd = kdev->base.fd;
   union kbase_ioctl_cs_get_glb_iface req = { 0 };

   /* First the counts, then group 0 and every stream (all groups are alike: kbase itself checks
    * group 0 for the stream count, csf/mali_kbase_csf.c kbase_csf_queue_bind). */
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_GET_GLB_IFACE, &req)) {
      mesa_loge("kbase: CS_GET_GLB_IFACE failed (%s)", strerror(errno));
      return -1;
   }

   uint32_t group_num = req.out.group_num;
   uint32_t stream_num = req.out.total_stream_num;
   if (!group_num || !stream_num) {
      mesa_loge("kbase: firmware reports %u groups and %u streams", group_num, stream_num);
      return -1;
   }

   struct basep_cs_group_control *groups = calloc(group_num, sizeof(*groups));
   struct basep_cs_stream_control *streams = calloc(stream_num, sizeof(*streams));
   if (!groups || !streams) {
      free(groups);
      free(streams);
      return -1;
   }

   req = (union kbase_ioctl_cs_get_glb_iface){
      .in = {
         .max_group_num = group_num,
         .max_total_stream_num = stream_num,
         .groups_ptr = (uintptr_t)groups,
         .streams_ptr = (uintptr_t)streams,
      },
   };
   int ret = kbase_kmod_ioctl(fd, KBASE_IOCTL_CS_GET_GLB_IFACE, &req);
   if (ret) {
      mesa_loge("kbase: CS_GET_GLB_IFACE failed (%s)", strerror(errno));
   } else {
      kdev->fw.glb_version = req.out.glb_version;
      kdev->fw.features = req.out.features;
      kdev->fw.group_num = req.out.group_num;
      kdev->fw.total_stream_num = req.out.total_stream_num;
      kdev->fw.group_features = groups[0].features;
      kdev->fw.stream_features = streams[0].features;

      /* STREAM_FEATURES: bits 7:0 the work registers minus one, bits 15:8 the scoreboards
       * (kbase csf/mali_kbase_csf_registers.h). */
      kdev->csif = (struct drm_panthor_csif_info){
         .csg_slot_count = req.out.group_num,
         .cs_slot_count = groups[0].stream_num,
         .cs_reg_count = (streams[0].features & 0xff) + 1,
         .scoreboard_slot_count = (streams[0].features >> 8) & 0xff,
         .unpreserved_cs_reg_count = KBASE_KMOD_RING_REG_COUNT,
      };
   }

   free(groups);
   free(streams);
   return ret;
}

static void
kbase_fill_props(struct kbase_kmod_dev *kdev, const struct kbase_props *raw)
{
   struct pan_kmod_dev_props *props = &kdev->base.props;
   const uint64_t *v = raw->value;
   uint32_t thread_features = v[KBASE_GPUPROP_RAW_THREAD_FEATURES];

   *props = (struct pan_kmod_dev_props){
      .gpu_id = v[KBASE_GPUPROP_RAW_GPU_ID],
      .gpu_variant = v[KBASE_GPUPROP_RAW_CORE_FEATURES] & 0xff,
      .shader_present = v[KBASE_GPUPROP_RAW_SHADER_PRESENT],
      .tiler_features = v[KBASE_GPUPROP_RAW_TILER_FEATURES],
      .mem_features = v[KBASE_GPUPROP_RAW_MEM_FEATURES],
      .mmu_features = v[KBASE_GPUPROP_RAW_MMU_FEATURES],
      .l2_features = v[KBASE_GPUPROP_RAW_L2_FEATURES],
      .texture_features = {
         v[KBASE_GPUPROP_RAW_TEXTURE_FEATURES_0],
         v[KBASE_GPUPROP_RAW_TEXTURE_FEATURES_1],
         v[KBASE_GPUPROP_RAW_TEXTURE_FEATURES_2],
         v[KBASE_GPUPROP_RAW_TEXTURE_FEATURES_3],
      },
      .max_threads_per_wg = v[KBASE_GPUPROP_RAW_THREAD_MAX_WORKGROUP_SIZE],
      .max_threads_per_core = v[KBASE_GPUPROP_RAW_THREAD_MAX_THREADS],
      /* THREAD_FEATURES: bits 21:0 registers per core, bits 31:24 tasks per core (as panthor). */
      .num_registers_per_core = thread_features & 0x3fffff,
      .max_tasks_per_core = thread_features >> 24,
      /* No THREAD_TLS_ALLOC on v10+: as many TLS instances as threads (as panthor). */
      .max_tls_instance_per_core = v[KBASE_GPUPROP_RAW_THREAD_MAX_THREADS],
      .afbc_features = 0,
      .gpu_can_query_timestamp = true,
      .timestamp_device_coherent = true,
      .pgsize_bitmap = PAN_PGSIZE_4K | PAN_PGSIZE_2M,
      .supported_bo_flags = PAN_KMOD_BO_FLAG_EXECUTABLE | PAN_KMOD_BO_FLAG_NO_MMAP |
                            PAN_KMOD_BO_FLAG_GPU_UNCACHED | PAN_KMOD_BO_FLAG_WB_MMAP |
                            PAN_KMOD_BO_FLAG_GPU_SYNC,
      .limits = {
         .single_gpu_mapping = true,
         .no_placed_cpu_mapping = true,
         .no_fd_sharing = true,
         .dma_buf_import = true,
      },
   };

   if (raw->present[KBASE_GPUPROP_RAW_THREAD_TLS_ALLOC] && v[KBASE_GPUPROP_RAW_THREAD_TLS_ALLOC])
      props->max_tls_instance_per_core = v[KBASE_GPUPROP_RAW_THREAD_TLS_ALLOC];

   /* COHERENCY_MODE 0 is ACE-Lite (I/O coherent), 1 full ACE; 31 none
    * (kbase gpu/mali_kbase_gpu_coherency.h). */
   kdev->coherency_mode = v[KBASE_GPUPROP_RAW_COHERENCY_MODE];
   props->is_io_coherent = kdev->coherency_mode == COHERENCY_ACE_LITE ||
                           kdev->coherency_mode == COHERENCY_ACE;
   kdev->gpu_available_memory = v[KBASE_GPUPROP_GPU_AVAILABLE_MEMORY_SIZE];

   /* kbase's GPU timestamp runs on the system counter (kbase keeps the GPU's TIMESTAMP_OFFSET
    * aligned with the CPU's; enabled by default from uAPI 1.37), so its frequency is CNTFRQ_EL0's. */
#if defined(__aarch64__)
   uint64_t freq;
   __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(freq));
   props->timestamp_frequency = freq;
#endif
   if (props->timestamp_frequency)
      props->timestamp_cycles_to_ns_factor =
         (double)NSEC_PER_SEC / props->timestamp_frequency;

   /* BASE_QUEUE_GROUP_PRIORITY_* are ordered high (0), medium, low, realtime (3); the kernel
    * answered with the highest it allows us. */
   switch (kdev->max_group_priority) {
   case BASE_QUEUE_GROUP_PRIORITY_REALTIME:
      props->allowed_group_priorities_mask |= PAN_KMOD_GROUP_ALLOW_PRIORITY_REALTIME;
      FALLTHROUGH;
   case BASE_QUEUE_GROUP_PRIORITY_HIGH:
      props->allowed_group_priorities_mask |= PAN_KMOD_GROUP_ALLOW_PRIORITY_HIGH;
      FALLTHROUGH;
   case BASE_QUEUE_GROUP_PRIORITY_MEDIUM:
      props->allowed_group_priorities_mask |= PAN_KMOD_GROUP_ALLOW_PRIORITY_MEDIUM;
      FALLTHROUGH;
   default:
      props->allowed_group_priorities_mask |= PAN_KMOD_GROUP_ALLOW_PRIORITY_LOW;
      break;
   }
}

/* Replaces the open file behind `fd` with a new open of the same device node. */
static int
kbase_reopen(int fd, const struct stat *st)
{
   char link[64], path[PATH_MAX];
   snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
   ssize_t n = readlink(link, path, sizeof(path) - 1);
   if (n <= 0)
      return -1;
   path[n] = '\0';

   int nfd = open(path, O_RDWR | O_CLOEXEC);
   if (nfd < 0)
      return -1;

   struct stat nst;
   int ret = -1;
   if (!fstat(nfd, &nst) && nst.st_rdev == st->st_rdev && dup3(nfd, fd, O_CLOEXEC) >= 0)
      ret = 0;
   close(nfd);
   return ret;
}

static struct pan_kmod_dev *
kbase_kmod_dev_create(int fd, uint32_t flags, const struct pan_kmod_allocator *allocator)
{
   /* Never send kbase ioctls to a DRM node (major 226): DRM dispatches on the ioctl number
    * alone and would run one of its own. */
   struct stat st;
   if (fstat(fd, &st) || !S_ISCHR(st.st_mode) || major(st.st_rdev) == 226)
      return NULL;

   /* The CSF handshake (ioctl 52). A JM kbase (uAPI major 11, handshake ioctl 0) fails here or
    * answers another major. */
   struct kbase_ioctl_version_check version = {
      .major = KBASE_KMOD_UAPI_MAJOR,
      .minor = KBASE_KMOD_UAPI_MINOR,
   };
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_VERSION_CHECK, &version)) {
      /* EPERM: this file already has a kbase context, because the fd is a dup() of another
       * device's (PanVK creates each VkDevice's pan_kmod_dev from a dup of the physical
       * device's fd). A kbase context is one GPU address space, so the new device gets its own:
       * the same node opened again, in place of the fd we were given. */
      if (errno != EPERM || kbase_reopen(fd, &st))
         return NULL;
      version = (struct kbase_ioctl_version_check){
         .major = KBASE_KMOD_UAPI_MAJOR,
         .minor = KBASE_KMOD_UAPI_MINOR,
      };
      if (kbase_kmod_ioctl(fd, KBASE_IOCTL_VERSION_CHECK, &version))
         return NULL;
   }
   if (version.major != KBASE_KMOD_UAPI_MAJOR) {
      mesa_logi("kbase: uAPI %u.%u is not CSF; not supported", version.major, version.minor);
      return NULL;
   }
   if (version.minor < 9) {
      mesa_loge("kbase: CSF uAPI 1.%u has no MEM_ALLOC_EX (needed from 1.9)", version.minor);
      return NULL;
   }

   struct kbase_ioctl_set_flags set_flags = { .create_flags = 0 };
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_SET_FLAGS, &set_flags)) {
      mesa_loge("kbase: SET_FLAGS failed (%s)", strerror(errno));
      return NULL;
   }

   /* kbase allocates tiler heap contexts and chunks itself, without SAME_VA, in the CUSTOM_VA
    * zone, which a 64-bit context only has once JIT is initialised: MEM_JIT_INIT carves it from
    * the top of the SAME_VA zone, and must come before any allocation
    * (kbase_region_tracker_init_jit). We never use JIT allocations themselves. */
   struct kbase_ioctl_mem_jit_init jit = {
      .va_pages = KBASE_KMOD_CUSTOM_VA_SIZE / sysconf(_SC_PAGESIZE),
      .phys_pages = KBASE_KMOD_CUSTOM_VA_SIZE / sysconf(_SC_PAGESIZE),
      .max_allocations = 1,
   };
   if (kbase_kmod_ioctl(fd, KBASE_IOCTL_MEM_JIT_INIT, &jit)) {
      mesa_loge("kbase: MEM_JIT_INIT failed (%s)", strerror(errno));
      return NULL;
   }

   struct kbase_kmod_dev *kdev = pan_kmod_alloc(allocator, sizeof(*kdev));
   if (!kdev)
      return NULL;

   kdev->base.fd = fd;
   kdev->uapi_major = version.major;
   kdev->uapi_minor = version.minor;
   kdev->page_size = sysconf(_SC_PAGESIZE);
   const char *debug = os_get_option("PAN_KBASE_DEBUG");
   kdev->debug_bo = debug && strstr(debug, "bo");

   /* The tracking page: required before any allocation until 1.18, harmless after. */
   kdev->tracking_page = os_mmap(NULL, kdev->page_size, PROT_NONE, MAP_SHARED, fd,
                                 BASE_MEM_MAP_TRACKING_HANDLE);
   if (kdev->tracking_page == MAP_FAILED) {
      mesa_loge("kbase: mapping the tracking page failed (%s)", strerror(errno));
      goto err_free;
   }

   /* The USER register page, for LATEST_FLUSH. */
   kdev->user_reg = os_mmap(NULL, kdev->page_size, PROT_READ, MAP_SHARED, fd,
                            BASEP_MEM_CSF_USER_REG_PAGE_HANDLE);
   if (kdev->user_reg == MAP_FAILED) {
      mesa_loge("kbase: mapping the USER register page failed (%s)", strerror(errno));
      kdev->user_reg = NULL;
      goto err_unmap;
   }

   struct kbase_props raw;
   if (kbase_read_props(fd, &raw))
      goto err_unmap;

   if (kbase_read_csf_iface(kdev))
      goto err_unmap;

   struct kbase_ioctl_context_priority_check prio = {
      .priority = BASE_QUEUE_GROUP_PRIORITY_REALTIME,
   };
   kdev->max_group_priority = kbase_kmod_ioctl(fd, KBASE_IOCTL_CONTEXT_PRIORITY_CHECK, &prio)
                                 ? BASE_QUEUE_GROUP_PRIORITY_MEDIUM
                                 : prio.priority;

   struct pan_kmod_driver driver = {
      .version = { .major = version.major, .minor = version.minor },
   };
   if (pan_kmod_dev_init(&kdev->base, fd, flags, &driver, allocator, &kbase_kmod_ops,
                         kbase_sync_provider_create(kdev)))
      goto err_unmap;

   kbase_fill_props(kdev, &raw);

   mesa_logi("kbase: CSF uAPI %u.%u, page size %u, gpu_id 0x%" PRIx64 " variant 0x%x, "
             "%u CSG slots x %u CS, %u CS registers, %u scoreboards, coherency %u",
             kdev->uapi_major, kdev->uapi_minor, kdev->page_size, kdev->base.props.gpu_id,
             kdev->base.props.gpu_variant, kdev->csif.csg_slot_count,
             kdev->csif.cs_slot_count, kdev->csif.cs_reg_count,
             kdev->csif.scoreboard_slot_count, kdev->coherency_mode);

   return &kdev->base;

err_unmap:
   if (kdev->user_reg)
      os_munmap(kdev->user_reg, kdev->page_size);
   os_munmap(kdev->tracking_page, kdev->page_size);
err_free:
   pan_kmod_free(allocator, kdev);
   return NULL;
}

static void
kbase_kmod_dev_destroy(struct pan_kmod_dev *dev)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(dev);

   kbase_csf_dev_finish(kdev);
   os_munmap(kdev->user_reg, kdev->page_size);
   os_munmap(kdev->tracking_page, kdev->page_size);
   pan_kmod_dev_cleanup(dev);
   pan_kmod_free(dev->allocator, kdev);
}

static struct pan_kmod_va_range
kbase_kmod_dev_query_user_va_range(const struct pan_kmod_dev *dev)
{
   /* The executable FIXED zone (PanVK's executable heap is the first 4 GiB of this range) and
    * the FIXED zone, clamped to the GPU's VA bits. */
   uint64_t end = MIN2(KBASE_FIXED_VA_END, 1ull << MMU_FEATURES_VA_BITS(dev->props.mmu_features));

   return (struct pan_kmod_va_range){
      .start = KBASE_EXEC_FIXED_VA_BASE,
      .size = end > KBASE_EXEC_FIXED_VA_BASE ? end - KBASE_EXEC_FIXED_VA_BASE : 0,
   };
}

static uint64_t
kbase_kmod_query_timestamp(const struct pan_kmod_dev *dev)
{
   union kbase_ioctl_get_cpu_gpu_timeinfo info = {
      .in = { .request_flags = BASE_TIMEINFO_TIMESTAMP_FLAG },
   };

   if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_GET_CPU_GPU_TIMEINFO, &info))
      return 0;

   return info.out.timestamp;
}

const struct drm_panthor_csif_info *
kbase_kmod_get_csif_props(const struct pan_kmod_dev *dev)
{
   return &kbase_kmod_dev(dev)->csif;
}

/* --- memory --------------------------------------------------------------------------------
 *
 * BASE_MEM_FIXED, the panthor model (PanVK picks each GPU VA): a BO is only a size and flags
 * until PanVK binds it; the bind allocates the pages with MEM_ALLOC_EX at that GPU VA, in the
 * EXEC_FIXED or FIXED zone, and the unbind frees them (MEM_FREE). kbase can map an allocation at
 * one GPU VA only, so a BO is bound once (props.limits.single_gpu_mapping). CPU mappings are
 * mmap()s of the device at the GPU VA (kbase_context_mmap's default case), made only when PanVK
 * maps, never at a chosen address (kbase refuses hints, thirdparty/mali_kbase_mmap.c).
 *
 * GPU_SYNC BOs hold sync objects command streams wait on. kbase re-evaluates the wait of a
 * suspended group only through a permanent kernel mapping of the sync object, which only CSF
 * event memory has (csf/mali_kbase_csf_scheduler.c evaluate_sync_update,
 * mali_kbase_mem_linux.c kbase_phy_alloc_mapping_get), and kbase forces that memory to SAME_VA.
 * So these BOs are allocated at once, their GPU VA is the CPU address of that first mapping
 * (pan_kmod_bo::fixed_va), the bind only checks it, and the BO's free unmaps (and so frees) it.
 *
 * Imported dma-bufs (kbase_kmod_bo_import) are SAME_VA too, and are placed the same way.
 */

struct kbase_kmod_bo {
   struct pan_kmod_bo base;
   /* The GPU VA of the allocation, 0 while unbound. */
   uint64_t gpu_va;
   /* The VM it is bound in. */
   struct kbase_kmod_vm *vm;
   /* The last CPU mapping handed out, for MEM_SYNC. */
   void *cpu;
   /* GPU_SYNC BOs and imported dma-bufs: the mapping of the SAME_VA region, whose address is
    * the GPU VA (fixed_va); unmapping it frees the region. */
   void *same_va_cpu;
   /* Imported dma-bufs: held GPU-mapped by a sticky resource mapping. */
   bool sticky;
};

struct kbase_kmod_vm {
   struct pan_kmod_vm base;
   simple_mtx_t lock;
   /* GPU VA -> struct kbase_kmod_bo * of every bound BO. */
   struct hash_table_u64 *bound;
};

static inline struct kbase_kmod_bo *
to_kbase_bo(struct pan_kmod_bo *bo)
{
   return container_of(bo, struct kbase_kmod_bo, base);
}

static inline struct kbase_kmod_vm *
to_kbase_vm(struct pan_kmod_vm *vm)
{
   return container_of(vm, struct kbase_kmod_vm, base);
}

/* BO handles are ours: kbase has none. 0 is never handed out. */
static uint32_t next_handle;

static struct pan_kmod_bo *
kbase_kmod_bo_alloc(struct pan_kmod_dev *dev, struct pan_kmod_vm *exclusive_vm, uint64_t size,
                    uint32_t flags)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(dev);

   if (flags & ~dev->props.supported_bo_flags) {
      mesa_loge("kbase: unsupported BO flags 0x%x", flags & ~dev->props.supported_bo_flags);
      return NULL;
   }

   struct kbase_kmod_bo *bo = pan_kmod_dev_alloc(dev, sizeof(*bo));
   if (!bo)
      return NULL;

   uint32_t handle = p_atomic_inc_return(&next_handle);
   size = align64(size, kdev->page_size);
   pan_kmod_bo_init(&bo->base, dev, exclusive_vm, size, flags, handle);

   if (flags & PAN_KMOD_BO_FLAG_GPU_SYNC) {
      uint64_t pages = size / kdev->page_size;
      union kbase_ioctl_mem_alloc req = {
         .in = {
            .va_pages = pages,
            .commit_pages = pages,
            .flags = BASE_MEM_CSF_EVENT | BASE_MEM_PROT_CPU_RD | BASE_MEM_PROT_CPU_WR |
                     BASE_MEM_PROT_GPU_RD | BASE_MEM_PROT_GPU_WR,
         },
      };
      if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_ALLOC, &req)) {
         mesa_loge("kbase: event memory allocation of %" PRIu64 " pages failed: %s", pages,
                   strerror(errno));
         goto err_free;
      }
      bo->same_va_cpu = os_mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, dev->fd,
                                req.out.gpu_va);
      if (bo->same_va_cpu == MAP_FAILED) {
         mesa_loge("kbase: mapping event memory failed: %s", strerror(errno));
         struct kbase_ioctl_mem_free free_req = { .gpu_addr = req.out.gpu_va };
         kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_FREE, &free_req);
         bo->same_va_cpu = NULL;
         goto err_free;
      }
      bo->base.fixed_va = (uintptr_t)bo->same_va_cpu;
   }

   return &bo->base;

err_free:
   pan_kmod_dev_free(dev, bo);
   return NULL;
}

static uint64_t
kbase_bo_alloc_flags(const struct kbase_kmod_bo *bo)
{
   uint32_t flags = bo->base.flags;
   uint64_t f = BASE_MEM_FIXED | BASE_MEM_PROT_GPU_RD;

   /* kbase refuses GPU-writable executable memory (kbase_check_alloc_flags). */
   if (flags & PAN_KMOD_BO_FLAG_EXECUTABLE)
      f |= BASE_MEM_PROT_GPU_EX;
   else
      f |= BASE_MEM_PROT_GPU_WR;

   if (!(flags & PAN_KMOD_BO_FLAG_NO_MMAP))
      f |= BASE_MEM_PROT_CPU_RD | BASE_MEM_PROT_CPU_WR;
   if (flags & PAN_KMOD_BO_FLAG_WB_MMAP)
      f |= BASE_MEM_CACHED_CPU;
   if (flags & PAN_KMOD_BO_FLAG_GPU_UNCACHED)
      f |= BASE_MEM_UNCACHED_GPU;
   /* GPU pages inner shareable, as panthor maps every BO: PanVK takes the shader cores' load/store
    * caches as coherent with each other and with L2 and cleans them only for the host. Without
    * this flag kbase maps pages non-shareable, and a write through one core's load/store cache
    * (a compute copy, a shader store) need not be seen by a read on another (the next draw's
    * vertex fetch). kbase allocates its own tiler heap chunks with the same flag. kbase takes
    * either this or COHERENT_SYSTEM (inner and outer shareable), not both
    * (kbase_check_alloc_flags). */
   if (bo->base.dev->props.is_io_coherent)
      f |= BASE_MEM_COHERENT_SYSTEM;
   else
      f |= BASE_MEM_COHERENT_LOCAL;

   return f;
}

static int
kbase_bo_bind(struct kbase_kmod_vm *vm, struct kbase_kmod_bo *bo, uint64_t va)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(bo->base.dev);
   uint64_t pages = bo->base.size / kdev->page_size;

   if (bo->gpu_va) {
      mesa_loge("kbase: BO %u is already bound at 0x%" PRIx64, bo->base.handle, bo->gpu_va);
      errno = EBUSY;
      return -1;
   }

   /* A BO whose unbind kbase refused still holds this VA (kbase_bo_unbind). */
   struct kbase_kmod_bo *holder = _mesa_hash_table_u64_search(vm->bound, va);
   if (holder) {
      mesa_loge("kbase: 0x%" PRIx64 " is still bound to BO %u", va, holder->base.handle);
      errno = EBUSY;
      return -1;
   }

   /* Event memory and imports already have their VA. */
   if (bo->base.fixed_va) {
      if (va != bo->base.fixed_va) {
         mesa_loge("kbase: BO %u lives at 0x%" PRIx64 ", not 0x%" PRIx64, bo->base.handle,
                   bo->base.fixed_va, va);
         errno = EINVAL;
         return -1;
      }
      bo->gpu_va = va;
      bo->vm = vm;
      _mesa_hash_table_u64_insert(vm->bound, va, bo);
      if (kdev->debug_bo)
         mesa_logi("kbase: bo %u bind 0x%" PRIx64 "-0x%" PRIx64 " flags 0x%x (SAME_VA)",
                   bo->base.handle, va, va + bo->base.size, bo->base.flags);
      return 0;
   }

   union kbase_ioctl_mem_alloc_ex req = {
      .in = {
         .va_pages = pages,
         .commit_pages = pages,
         .flags = kbase_bo_alloc_flags(bo),
         .fixed_address = va,
      },
   };
   if (kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_MEM_ALLOC_EX, &req)) {
      mesa_loge("kbase: MEM_ALLOC_EX of %" PRIu64 " pages at 0x%" PRIx64 " (flags 0x%" PRIx64
                ") failed: %s", pages, va, (uint64_t)req.in.flags, strerror(errno));
      return -1;
   }

   bo->gpu_va = va;
   bo->vm = vm;
   _mesa_hash_table_u64_insert(vm->bound, va, bo);
   if (kdev->debug_bo)
      mesa_logi("kbase: bo %u bind 0x%" PRIx64 "-0x%" PRIx64 " flags 0x%x", bo->base.handle, va,
                va + bo->base.size, bo->base.flags);
   return 0;
}

/* Frees [bo]'s GPU mapping. When kbase refuses (the region, and so the VA, stays), the error is
 * returned (-1, errno) and the BO stays bound, so that no other BO is bound at that VA and a later
 * unbind tries again; with [forget], as when the BO itself goes, it is forgotten all the same. */
static int
kbase_bo_unbind(struct kbase_kmod_bo *bo, bool forget)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(bo->base.dev);
   struct kbase_ioctl_mem_free req = { .gpu_addr = bo->gpu_va };
   int ret = 0;

   if (kdev->debug_bo)
      mesa_logi("kbase: bo %u unbind 0x%" PRIx64 "-0x%" PRIx64, bo->base.handle, bo->gpu_va,
                bo->gpu_va + bo->base.size);

   /* Event memory and imports stay until the BO is freed. */
   if (!bo->base.fixed_va && kbase_kmod_ioctl(kdev->base.fd, KBASE_IOCTL_MEM_FREE, &req)) {
      int err = errno;
      mesa_loge("kbase: MEM_FREE at 0x%" PRIx64 " failed: %s", bo->gpu_va, strerror(err));
      errno = err;
      ret = -1;
      if (!forget)
         return ret;
   }

   _mesa_hash_table_u64_remove(bo->vm->bound, bo->gpu_va);
   bo->gpu_va = 0;
   bo->vm = NULL;
   return ret;
}

static void
kbase_kmod_bo_free(struct pan_kmod_bo *pbo)
{
   struct kbase_kmod_bo *bo = to_kbase_bo(pbo);

   if (bo->gpu_va) {
      struct kbase_kmod_vm *vm = bo->vm;
      simple_mtx_lock(&vm->lock);
      kbase_bo_unbind(bo, true);
      simple_mtx_unlock(&vm->lock);
   }

   if (bo->sticky) {
      uint64_t va = pbo->fixed_va;
      struct kbase_ioctl_sticky_resource_unmap req = { .count = 1, .address = (uintptr_t)&va };
      if (kbase_kmod_ioctl(pbo->dev->fd, KBASE_IOCTL_STICKY_RESOURCE_UNMAP, &req))
         mesa_loge("kbase: STICKY_RESOURCE_UNMAP at 0x%" PRIx64 " failed: %s", va,
                   strerror(errno));
   }

   /* Unmapping the SAME_VA mapping frees the event memory or the import. */
   if (bo->same_va_cpu)
      os_munmap(bo->same_va_cpu, pbo->size);

   pan_kmod_bo_cleanup(pbo);
   pan_kmod_dev_free(pbo->dev, bo);
}

/* Imports a dma-buf (the app's AHardwareBuffers that Mesa's WSI blits frames into). For a 64-bit
 * client kbase imports into the SAME_VA zone (mali_kbase_mem_linux.c kbase_mem_from_umm) and
 * answers with a cookie; the mmap() of the cookie gives the region its GPU VA, the mapping's CPU
 * address (kbasep_reg_mmap), which PanVK binds as the BO's fixed_va, as for event memory. The
 * mapping is PROT_NONE: it only holds the VA (CPU access to an import is not offered: NO_MMAP), and
 * its munmap frees the region. A sticky resource mapping keeps the pages on the GPU on a kernel
 * that maps dma-bufs only for the jobs that use them (CONFIG_MALI_DMA_BUF_MAP_ON_DEMAND: CSF jobs
 * name none); elsewhere the import has mapped them and it only counts a reference
 * (kbase_mem_umm_map). Imports are not deduplicated: kbase maps each one on its own. */
static struct pan_kmod_bo *
kbase_kmod_bo_import(struct pan_kmod_dev *dev, int fd)
{
   struct kbase_kmod_dev *kdev = kbase_kmod_dev(dev);
   int dma_buf = fd;

   union kbase_ioctl_mem_import req = {
      .in = {
         /* GPU pages inner shareable, as for our own allocations (kbase_bo_alloc_flags). */
         .flags = BASE_MEM_PROT_GPU_RD | BASE_MEM_PROT_GPU_WR |
                  (dev->props.is_io_coherent ? BASE_MEM_COHERENT_SYSTEM
                                             : BASE_MEM_COHERENT_LOCAL),
         .phandle = (uintptr_t)&dma_buf,
         .type = BASE_MEM_IMPORT_TYPE_UMM,
      },
   };
   if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_IMPORT, &req)) {
      mesa_loge("kbase: MEM_IMPORT of dma-buf %d failed: %s", fd, strerror(errno));
      return NULL;
   }

   const uint64_t cookie = req.out.gpu_va;
   const uint64_t size = req.out.va_pages * kdev->page_size;
   struct kbase_ioctl_mem_free free_req = { .gpu_addr = cookie };
   if (!(req.out.flags & BASE_MEM_NEED_MMAP)) {
      /* Not SAME_VA: a 32-bit client. */
      mesa_loge("kbase: MEM_IMPORT placed dma-buf %d outside SAME_VA", fd);
      kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_FREE, &free_req);
      errno = ENOTSUP;
      return NULL;
   }

   /* pan_kmod_bo_import() holds the handle table's lock; pan_kmod_bo_put() empties the slot. */
   const uint32_t handle = p_atomic_inc_return(&next_handle);
   struct kbase_kmod_bo *bo = pan_kmod_dev_alloc(dev, sizeof(*bo));
   struct pan_kmod_bo **slot =
      bo ? util_sparse_array_get(&dev->handle_to_bo.array, handle) : NULL;
   if (!slot) {
      /* A pending cookie is freed by MEM_FREE (kbase_mem_free). */
      kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_FREE, &free_req);
      if (bo)
         pan_kmod_dev_free(dev, bo);
      errno = ENOMEM;
      return NULL;
   }

   void *cpu = os_mmap(NULL, size, PROT_NONE, MAP_SHARED, dev->fd, cookie);
   if (cpu == MAP_FAILED) {
      mesa_loge("kbase: mapping the import of dma-buf %d failed: %s", fd, strerror(errno));
      /* A mapping that fails after kbase took the cookie (kbase_cpu_mmap's allocation) leaves the
       * region to the context's end: say so. */
      if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_FREE, &free_req))
         mesa_loge("kbase: freeing the import's cookie failed: %s", strerror(errno));
      pan_kmod_dev_free(dev, bo);
      return NULL;
   }

   uint64_t va = (uintptr_t)cpu;
   struct kbase_ioctl_sticky_resource_map map = { .count = 1, .address = (uintptr_t)&va };
   if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_STICKY_RESOURCE_MAP, &map)) {
      mesa_loge("kbase: STICKY_RESOURCE_MAP of dma-buf %d at 0x%" PRIx64 " failed: %s", fd, va,
                strerror(errno));
      os_munmap(cpu, size);
      pan_kmod_dev_free(dev, bo);
      return NULL;
   }

   pan_kmod_bo_init(&bo->base, dev, NULL, size,
                    PAN_KMOD_BO_FLAG_IMPORTED | PAN_KMOD_BO_FLAG_NO_MMAP, handle);
   bo->base.fixed_va = va;
   bo->same_va_cpu = cpu;
   bo->sticky = true;
   *slot = &bo->base;
   if (kdev->debug_bo)
      mesa_logi("kbase: bo %u import of dma-buf %d at 0x%" PRIx64 "-0x%" PRIx64, handle, fd, va,
                va + size);
   return &bo->base;
}

static int
kbase_kmod_bo_export(struct pan_kmod_bo *bo)
{
   errno = ENOTSUP;
   return -1;
}

static void *
kbase_kmod_bo_mmap(struct pan_kmod_bo *pbo, int prot, int flags, void *host_addr)
{
   struct kbase_kmod_bo *bo = to_kbase_bo(pbo);

   if (!bo->gpu_va || host_addr || (flags & MAP_FIXED)) {
      errno = EINVAL;
      return MAP_FAILED;
   }

   void *cpu = os_mmap(NULL, pbo->size, prot, flags, pbo->dev->fd, bo->gpu_va);
   if (cpu != MAP_FAILED)
      bo->cpu = cpu;
   return cpu;
}

/* Kernel cache maintenance, for when user-space cache ops are not used
 * (PAN_KMOD_DEV_FLAG_MMAP_SYNC_THROUGH_KERNEL, or no util cache ops). */
static int
kbase_kmod_flush_bo_map_syncs(struct pan_kmod_dev *dev)
{
   int ret = 0;

   util_dynarray_foreach(&dev->pending_bo_syncs.array, struct pan_kmod_deferred_bo_sync, sync) {
      struct kbase_kmod_bo *bo = to_kbase_bo(sync->bo);
      if (!bo->gpu_va || !bo->cpu)
         continue;

      struct kbase_ioctl_mem_sync req = {
         .gpu_va = bo->gpu_va + sync->start,
         .user_addr = (uintptr_t)bo->cpu + sync->start,
         .size = sync->size,
         .type = BASE_SYNCSET_OP_MSYNC,
      };
      if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_SYNC, &req))
         ret = -1;
      if (sync->type == PAN_KMOD_BO_SYNC_CPU_CACHE_FLUSH_AND_INVALIDATE) {
         req.type = BASE_SYNCSET_OP_CSYNC;
         if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_MEM_SYNC, &req))
            ret = -1;
      }
   }

   return ret;
}

static bool
kbase_kmod_bo_wait(struct pan_kmod_bo *bo, int64_t timeout_ns, bool for_read_only_access)
{
   /* PanVK on CSF waits on syncs, not on BOs. */
   return true;
}

static struct pan_kmod_vm *
kbase_kmod_vm_create(struct pan_kmod_dev *dev, uint32_t flags, uint64_t va_start,
                     uint64_t va_range)
{
   /* The kbase context is the address space: one VM per device. PanVK picks every address. */
   if (flags & PAN_KMOD_VM_FLAG_AUTO_VA) {
      mesa_loge("kbase: VMs with kernel-chosen addresses are not supported");
      return NULL;
   }

   struct kbase_kmod_vm *vm = pan_kmod_dev_alloc(dev, sizeof(*vm));
   if (!vm)
      return NULL;

   vm->bound = _mesa_hash_table_u64_create(NULL);
   if (!vm->bound) {
      pan_kmod_dev_free(dev, vm);
      return NULL;
   }

   simple_mtx_init(&vm->lock, mtx_plain);
   pan_kmod_vm_init(&vm->base, dev, 0, flags);
   return &vm->base;
}

static void
kbase_kmod_vm_destroy(struct pan_kmod_vm *pvm)
{
   struct kbase_kmod_vm *vm = to_kbase_vm(pvm);

   /* Free whatever is still bound. */
   struct util_dynarray left;
   util_dynarray_init(&left, NULL);
   hash_table_u64_foreach(vm->bound, entry)
      util_dynarray_append(&left, (struct kbase_kmod_bo *)entry.data);
   util_dynarray_foreach(&left, struct kbase_kmod_bo *, bo)
      kbase_bo_unbind(*bo, true);
   util_dynarray_fini(&left);

   pan_kmod_vm_cleanup(pvm);
   _mesa_hash_table_u64_destroy(vm->bound);
   simple_mtx_destroy(&vm->lock);
   pan_kmod_dev_free(pvm->dev, vm);
}

static int
kbase_vm_unmap_range(struct kbase_kmod_vm *vm, uint64_t start, uint64_t size)
{
   /* PanVK unmaps a BO's whole range; free every BO bound inside it. A refused free fails the
    * unmap (the others still go). */
   struct kbase_kmod_bo *bo = _mesa_hash_table_u64_search(vm->bound, start);
   if (bo && bo->base.size >= size)
      return kbase_bo_unbind(bo, false);

   int ret = 0, err = 0;
   struct util_dynarray hits;
   util_dynarray_init(&hits, NULL);
   hash_table_u64_foreach(vm->bound, entry) {
      if (entry.key >= start && entry.key < start + size)
         util_dynarray_append(&hits, (struct kbase_kmod_bo *)entry.data);
   }
   util_dynarray_foreach(&hits, struct kbase_kmod_bo *, hit) {
      if (kbase_bo_unbind(*hit, false)) {
         ret = -1;
         err = errno;
      }
   }
   util_dynarray_fini(&hits);
   if (ret)
      errno = err;
   return ret;
}

static int
kbase_kmod_vm_bind(struct pan_kmod_vm *pvm, enum pan_kmod_vm_op_mode mode,
                   struct pan_kmod_vm_op *ops, uint32_t op_count)
{
   struct kbase_kmod_vm *vm = to_kbase_vm(pvm);
   struct util_sync_provider *sync = pvm->dev->sync_ops;
   int ret = 0;

   /* kbase binds synchronously. Asynchronous requests wait for their syncs on the CPU first
    * and signal theirs afterwards. */
   for (uint32_t i = 0; i < op_count; i++) {
      struct pan_kmod_vm_op *op = &ops[i];

      if (pan_kmod_vm_op_check(pvm, mode, op))
         return -1;

      for (uint32_t w = 0; w < op->wait.count; w++) {
         uint32_t handle = op->wait.array[w].handle;
         uint64_t point = op->wait.array[w].point;
         if (sync->timeline_wait(sync, &handle, &point, 1, INT64_MAX,
                                 DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT, NULL))
            return -1;
      }

      simple_mtx_lock(&vm->lock);
      switch (op->type) {
      case PAN_KMOD_VM_OP_TYPE_MAP:
         if (op->va.start == PAN_KMOD_VM_MAP_AUTO_VA || op->map.bo_offset ||
             op->va.size != op->map.bo->size) {
            mesa_loge("kbase: only whole-BO binds at a given VA are supported");
            errno = EINVAL;
            ret = -1;
         } else {
            ret = kbase_bo_bind(vm, to_kbase_bo(op->map.bo), op->va.start);
         }
         break;
      case PAN_KMOD_VM_OP_TYPE_UNMAP:
         ret = kbase_vm_unmap_range(vm, op->va.start, op->va.size);
         break;
      case PAN_KMOD_VM_OP_TYPE_SYNC_ONLY:
         break;
      }
      simple_mtx_unlock(&vm->lock);

      if (ret)
         return ret;

      for (uint32_t s = 0; s < op->signal.count; s++) {
         uint32_t handle = op->signal.array[s].handle;
         uint64_t point = op->signal.array[s].point;
         if (point ? sync->timeline_signal(sync, &handle, &point, 1)
                   : sync->signal(sync, &handle, 1))
            return -1;
      }
   }

   return 0;
}

/* The CPU changed sync objects (a host vkSetEvent): kbase re-evaluates the waits of suspended
 * groups and signals the firmware (csf/mali_kbase_csf_event.c kbase_csf_event_signal). */
static void
kbase_kmod_notify_sync_update(struct pan_kmod_dev *dev)
{
   if (kbase_kmod_ioctl(dev->fd, KBASE_IOCTL_CS_EVENT_SIGNAL, NULL))
      mesa_loge("kbase: CS_EVENT_SIGNAL failed: %s", strerror(errno));
}

static uint32_t
kbase_kmod_get_flush_id(const struct pan_kmod_dev *dev)
{
   const struct kbase_kmod_dev *kdev = kbase_kmod_dev(dev);

   /* LATEST_FLUSH, offset 0 of the USER register page (kbase uAPI
    * gpu/backend/mali_kbase_gpu_regmap_csf.h). */
   return kdev->user_reg ? *(volatile const uint32_t *)kdev->user_reg : 0;
}

const struct pan_kmod_ops kbase_kmod_ops = {
   .dev_create = kbase_kmod_dev_create,
   .dev_destroy = kbase_kmod_dev_destroy,
   .dev_query_user_va_range = kbase_kmod_dev_query_user_va_range,
   .bo_alloc = kbase_kmod_bo_alloc,
   .bo_free = kbase_kmod_bo_free,
   .bo_import = kbase_kmod_bo_import,
   .bo_export = kbase_kmod_bo_export,
   .bo_mmap = kbase_kmod_bo_mmap,
   .flush_bo_map_syncs = kbase_kmod_flush_bo_map_syncs,
   .bo_wait = kbase_kmod_bo_wait,
   .vm_create = kbase_kmod_vm_create,
   .vm_destroy = kbase_kmod_vm_destroy,
   .vm_bind = kbase_kmod_vm_bind,
   .query_timestamp = kbase_kmod_query_timestamp,
   .ioctl = kbase_kmod_csf_ioctl,
   .get_csif_props = kbase_kmod_get_csif_props,
   .get_flush_id = kbase_kmod_get_flush_id,
   .notify_sync_update = kbase_kmod_notify_sync_update,
};
