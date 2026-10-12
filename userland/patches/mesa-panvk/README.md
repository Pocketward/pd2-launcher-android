# Mesa patches for PanVK on kbase

Applied by `build-panvk.sh`, after `../mesa/`, to the Mesa tree pinned by `config.sh` (`PANVK_MESA_*`). The kbase pan_kmod backend's own files are not patches: they live in `../../mesa-kbase/` and are copied over the patched tree.

- `0001-panfrost-kmod-kbase-backend-and-hooks.patch`: registers and builds the kbase backend in `src/panfrost/lib/kmod`; adds optional `pan_kmod_ops` hooks (panthor command-stream ioctls, CSF interface info, flush id, sync-update notify), `pan_kmod_dev_props.limits`, the `GPU_SYNC` BO flag and `fixed_va`.
- `0002-panvk-run-on-kbase.patch`: PanVK enumerates `/dev/mali0` and runs on the kbase backend: kmod accessors in the CSF queue code, VA heaps from the backend's range, GPU-map before CPU-map, no mirror mapping, sparse binding, placed maps or fd sharing where the backend has none, waited-on sync objects in `GPU_SYNC` memory, `vkSetEvent` notifies the kernel.
- `0004-panfrost-crc-clear-colour-hash.patch`: a 64-bit mixing hash for the CRC clear colour, so different clear values do not collide and transaction elimination does not skip a clear.
