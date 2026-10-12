# Mesa patches (Vulkan WSI)

Applied by `build-turnip.sh` to mesa-26.2.3 (`config.sh` pins the tarball), and by `build-panvk.sh` to PanVK's Mesa (`PANVK_MESA_*`) before `../mesa-panvk/`. They touch only `src/vulkan/wsi`, which every Mesa Vulkan driver shares.

- `0001-wsi-sw-reads-cached-memory.patch`: the software X11 present (`MESA_VK_WSI_DEBUG=sw`) reads each frame from host-cached memory, non-coherent if need be with `vkInvalidateMappedMemoryRanges` before each read, instead of write-combined memory.
- `0002-wsi-sw-present-counter.patch`: when `PD2_PRESENT_COUNTER` names a file, the software present increments its first 32-bit word after each frame has reached the X server.
- `0003-wsi-sw-paced-by-the-game-view.patch`: the software present waits, at most 100 ms, until the Game view has taken the previous frame off the X screen (signalled in the present counter's page), in every present mode.
- `0004-wsi-sw-direct-present-to-the-app.patch`: when `PD2_PRESENT_SOCKET` names the app's socket, the swapchain imports the app's AHardwareBuffers, blits each frame into a free one and sends `PRESENT` instead of `PutImage`; any setup failure keeps `PutImage`.
