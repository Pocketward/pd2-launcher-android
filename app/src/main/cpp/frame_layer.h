// The Game view's child layer (Android 10+, frame_layer.cpp): the frame copy's buffers set on a
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
// [frame_rate] frames a second (Android 11+; 0: no vote); NULL when there is none.
struct frame_layer *frame_layer_create(ANativeWindow *window, int buffer_w, int buffer_h, int transform, float frame_rate);

// The next free buffer, locked for writing: its pixels and stride in pixels. False when the
// compositor still holds every buffer (try again on the next display frame).
int frame_layer_lock(struct frame_layer *layer, uint32_t **pixels, int *stride);

// Unlocks the buffer frame_layer_lock gave and shows it.
void frame_layer_post(struct frame_layer *layer);

// Takes the layer off the screen and frees it once the compositor has answered every frame.
void frame_layer_destroy(struct frame_layer *layer);

#ifdef __cplusplus
}
#endif
