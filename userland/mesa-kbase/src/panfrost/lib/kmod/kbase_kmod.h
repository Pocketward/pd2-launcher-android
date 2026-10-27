/*
 * pan_kmod backend for Arm's mali_kbase kernel driver (/dev/mali0), CSF GPUs (Mali v10+): shared
 * declarations of the backend's three files (kbase_kmod.c: device and memory, kbase_kmod_sync.c:
 * the sync provider, kbase_kmod_csf.c: queue groups).
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "drm-uapi/panthor_drm.h"

#include "util/macros.h"

#include "pan_kmod.h"

/* The newest kbase CSF uAPI version this backend implements; the kernel answers with the lower of
 * this and its own, and keeps to that version's behaviour (kbase_api_handshake). */
#define KBASE_KMOD_UAPI_MAJOR 1
#define KBASE_KMOD_UAPI_MINOR 38

/* Registers at the top of each CS's register file that our ring code uses around a call into a
 * userspace stream (as panthor's DRM_PANTHOR_IOCTL_GROUP_SUBMIT does). */
#define KBASE_KMOD_RING_REG_COUNT 4

struct kbase_sync;
struct kbase_csf;

struct kbase_kmod_dev {
   struct pan_kmod_dev base;

   /* Negotiated uAPI version. */
   uint16_t uapi_major;
   uint16_t uapi_minor;

   /* The kernel's page size: kbase handles and cookies are in kernel pages. */
   uint32_t page_size;

   /* The kernel has no MEM_ALLOC_EX (the 1.9 memory interface), whatever uAPI minor it reports
    * (MediaTek's r32p1 kbase reports 1.18 without it): the kernel places every BO at its
    * allocation, and PanVK binds it there (pan_kmod_bo::fixed_va). */
   bool kernel_places_bos;

   /* The kernel takes only these older layouts (found when the negotiated version's layout gets
    * ENOTTY): CS_QUEUE_GROUP_CREATE_1_6, which has no CSI exception handlers, and
    * CS_TILER_HEAP_INIT_1_13. */
   bool group_create_1_6;
   bool tiler_heap_init_1_13;

   /* The mapping of BASE_MEM_MAP_TRACKING_HANDLE (required before 1.18). */
   void *tracking_page;

   /* The USER register page (BASEP_MEM_CSF_USER_REG_PAGE_HANDLE): LATEST_FLUSH. */
   void *user_reg;

   /* CSF interface: global version and features, and the panthor-shaped summary PanVK reads. */
   struct {
      uint32_t glb_version;
      uint32_t features;
      uint32_t group_num;
      uint32_t total_stream_num;
      uint32_t group_features;
      uint32_t stream_features;
   } fw;
   struct drm_panthor_csif_info csif;

   /* Raw properties kept for diagnostics. */
   uint32_t coherency_mode;
   uint64_t gpu_available_memory;

   /* Group priority allowed by the kernel for this process (BASE_QUEUE_GROUP_PRIORITY_*). */
   uint8_t max_group_priority;

   /* PAN_KBASE_DEBUG=bo: log every BO bind and unbind (GPU VA ranges, to place a fault address). */
   bool debug_bo;

   /* Shared state of the sync provider (kbase_kmod_sync.c). */
   struct kbase_sync *sync;

   /* Queue groups and tiler heaps (kbase_kmod_csf.c), created on first use. */
   struct kbase_csf *csf;
};

static inline struct kbase_kmod_dev *
kbase_kmod_dev(const struct pan_kmod_dev *dev)
{
   return container_of(dev, struct kbase_kmod_dev, base);
}

static inline bool
kbase_kmod_uapi_at_least(const struct kbase_kmod_dev *kdev, uint16_t minor)
{
   return kdev->uapi_major > KBASE_KMOD_UAPI_MAJOR ||
          (kdev->uapi_major == KBASE_KMOD_UAPI_MAJOR && kdev->uapi_minor >= minor);
}

/* ioctl() retried on EINTR/EAGAIN; returns its result with errno set on failure. */
int kbase_kmod_ioctl(int fd, unsigned long request, void *arg);

/* The sync provider over a kbase device (kbase_kmod_sync.c). */
struct util_sync_provider *kbase_sync_provider_create(struct kbase_kmod_dev *kdev);

/* Reads and acts on pending kbase notifications (kbase_kmod_sync.c). */
void kbase_sync_drain_notifications(struct kbase_sync *sync);

/* The panthor command-stream ioctls on kbase (kbase_kmod_csf.c). */
int kbase_kmod_csf_ioctl(struct pan_kmod_dev *dev, unsigned long request, void *arg);
void kbase_csf_dev_finish(struct kbase_kmod_dev *kdev);

/* CSF interface information in panthor's layout, for PanVK's CSF code. */
const struct drm_panthor_csif_info *
kbase_kmod_get_csif_props(const struct pan_kmod_dev *dev);

extern const struct pan_kmod_ops kbase_kmod_ops;
