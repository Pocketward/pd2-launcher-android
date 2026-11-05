# WSI patches for RADV for Xclipse (radv-xclipse)

Applied by `build-radv.sh`, in name order and exactly (`--fuzz=0`, no offset), to the radv-xclipse tree that `config.sh` pins (`RADV_XC_COMMIT`). They are `../mesa/0001`–`0004` and `0006`–`0008` (see `../mesa/README.md` for what each one does), ported to that tree; `0005` is OpenGL only and has no part in a Vulkan driver.

The port:

- The tree's X11 swapchains carry their own software flag, `wsi_swapchain::sw` (`wsi->sw`, or the tree's `x11_sw_fallback` without DRI3, which its RADV sets for the Xclipse outside Android). Each `chain->base.wsi->sw` of the patches (added and context lines) is `chain->base.sw`; 0004's `!wsi->sw` in `sw_direct_open` is `!chain->base.sw`; 0006 uses `swapchain->sw` in `wsi_common_queue_present` and sets `present_thread_waits_fences` from `chain->base.sw`. The app sets `MESA_VK_WSI_DEBUG=sw`, so both flags are true there.
- 0007's new fields of `struct x11_swapchain` follow `window`: the tree has no `event_conn` (upstream added it after the tree's base).
- Otherwise each patch has the hunks and the changed lines of its original, each hunk in the same function. So 0008 has the rules of `../mesa/0008`: a present of every mode (vsync on or off) waits for the app's clock, at most one presented image waits for it (`sw_clock_acquire`), and Detailed logging's frame times (`sw_stamp`).

`build-mali-panvk.sh` applies the same patches, in the same way, to the mali-panvk tree that `config.sh` pins (`MALI_PANVK_COMMIT`): that tree's four WSI files (`src/vulkan/wsi/wsi_common.c`, `wsi_common.h`, `wsi_common_private.h`, `wsi_common_x11.c`) are byte-identical to radv-xclipse's at `RADV_XC_COMMIT`.

When `../mesa/` changes one of these patches, port the change here too. When `RADV_XC_COMMIT` or `MALI_PANVK_COMMIT` moves, refresh them for the new tree: `build-radv.sh` and `build-mali-panvk.sh` refuse a patch that needs an offset. If the two trees' WSI files stop being the same, mali-panvk needs a patch folder of its own.
