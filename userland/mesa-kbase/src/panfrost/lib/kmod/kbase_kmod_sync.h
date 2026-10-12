/*
 * The kbase sync provider's interface to the queue code (kbase_kmod_csf.c): fences made of GPU
 * sync-object conditions, attached to and read from syncobjs. See kbase_kmod_sync.c.
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

struct kbase_sync;
struct kbase_fence;

/* "The 64-bit GPU sync object at value_ptr (CPU address, CSF event memory) reached value."
 * group_handle names the kbase queue group whose fatal error also ends the wait (or >= 256). */
struct kbase_fence_cond {
   const uint64_t *value_ptr;
   uint64_t gpu_va;
   uint64_t value;
   uint32_t group_handle;
};

/* A new fence holding `count` conditions (NULL, i.e. signalled, for count 0). */
struct kbase_fence *kbase_fence_create(const struct kbase_fence_cond *conds, uint32_t count);
void kbase_fence_unref(struct kbase_fence *f);
const struct kbase_fence_cond *kbase_fence_conds(const struct kbase_fence *f, uint32_t *count);

/* Makes `fence` the payload of `handle` (point 0) or appends it at `point`; takes a reference. */
int kbase_sync_attach(struct kbase_sync *sync, uint32_t handle, uint64_t point,
                      struct kbase_fence *fence);

/* The fence covering `point` of `handle` (NULL when signalled; a reference to drop with
 * kbase_fence_unref). Fails with EINVAL when nothing covers that point yet. */
int kbase_sync_get_fence(struct kbase_sync *sync, uint32_t handle, uint64_t point,
                         struct kbase_fence **out);

bool kbase_sync_group_fatal(const struct kbase_sync *sync, uint8_t group);
void kbase_sync_group_reset(struct kbase_sync *sync, uint8_t group);

/* `hook` is called, by whichever thread reads kbase's notifications and without the provider's
 * lock, once for each group newly marked fatal (NULL: no hook). */
void kbase_sync_set_fatal_hook(struct kbase_sync *sync, void (*hook)(void *data, uint8_t group),
                               void *data);
