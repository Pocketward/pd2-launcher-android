// The Game view's child layer (frame_layer.cpp): the frame copy's buffers set on a
// child of the window's surface by transactions, not queued through the window, whose
// queueBuffer some vendors pace below the display's rate.
#pragma once

#include <android/native_window.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct frame_layer;

// A child layer of [window] showing [buffer_w]x[buffer_h] RGBX buffers stored turned by
// [transform] (ANATIVEWINDOW_TRANSFORM_*), scaled to the window's size, asking the display for
// [frame_rate] frames a second (0: no vote); NULL when there is none. One buffer is allocated
// now, the others when they are first locked.
struct frame_layer *frame_layer_create(ANativeWindow *window, int buffer_w, int buffer_h, int transform, float frame_rate);

// The next free buffer, locked for writing: its pixels and stride in pixels (1). 0 when the
// compositor still holds it (try again on the next display frame); -1 when it is not allocated
// and the allocator refuses it (frame_layer_rest then asks for a frame again; the allocator is
// asked again 100 ms later at the soonest).
int frame_layer_lock(struct frame_layer *layer, uint32_t **pixels, int *stride);

// The layer asks the display for [frame_rate] frames a second from now on (0: no vote): at once
// by a transaction of its own, and with its first frame if none showed yet. Any thread.
void frame_layer_set_rate(struct frame_layer *layer, float frame_rate);

// Unlocks the buffer frame_layer_lock gave and shows it.
void frame_layer_post(struct frame_layer *layer);

// The window has a new size, [dest_w]x[dest_h]: the layer keeps its buffers and shows them scaled
// to it, at once by a transaction of its own (the copy may rest), and again with the next frame.
// A frame that read the older size applies its transaction first, so the new size is the last one
// set. Any thread but the copy's.
void frame_layer_resize(struct frame_layer *layer, int dest_w, int dest_h);

// Call on every display frame ([frame_ns]): [resting] while the copy shows nothing, the game's
// own buffers covering the whole view. After 5 s of rest without a break, the buffers that are not
// on the screen go once the compositor no longer holds them (the one on the screen stays under the
// game's); frame_layer_lock allocates them again. True from then (or from a refused allocation)
// until the next frame_layer_post: the copy shows the X screen again at once, whatever changed.
int frame_layer_rest(struct frame_layer *layer, int64_t frame_ns, int resting);

// Takes the layer off the screen, lets it go, and frees the buffers and the rest once the
// compositor has answered every frame; when it has not answered within 1 s, its last answer
// frees them.
void frame_layer_destroy(struct frame_layer *layer);

#ifdef __cplusplus
}
#endif
