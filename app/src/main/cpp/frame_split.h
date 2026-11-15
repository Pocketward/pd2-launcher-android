// The split of the time of each frame that the direct present shows, for the service log: where a
// frame spends its time between D2GL's render thread and the screen, apart from the Android calls
// so that the build host can test it (app/src/test/cpp/frame_split_test.c). The direct present
// (present_server.cpp) keeps one. The times are those of the pointer split (pointer_split.h,
// struct split_frame); this split takes every frame shown, with or without pointer moves.
#pragma once
#include <stdint.h>
#include "pointer_split.h"
#ifdef __cplusplus
extern "C" {
#endif

// The parts of a frame, in the summary's order.
enum frame_part {
    FRAME_PRESENT_ACQUIRE, // the render thread's previous present to this acquire: its time between frames
    FRAME_ACQUIRE_PRESENT, // the acquire to the present: the render thread's frame
    FRAME_PRESENT_GPU,     // the present to GPU done: the GPU's rest of the frame
    FRAME_PRESENT_CLOCK,   // the present to the clock: the GPU's rest, then our pacing
    FRAME_CLOCK_APPLY,     // the clock to the transaction: the app
    FRAME_APPLY_SCREEN,    // the transaction to the screen: SurfaceFlinger and the display
    FRAME_CLOCK_LATCH,     // the clock to the latch: our tick's phase against the display's latch
    FRAME_PARTS,
};

// A window holds at most this many frames (more than 60 s at 60 fps).
#define FRAME_SAMPLES 4096

// One window's summary: from its first frame to its last ([span_ns]), the [frames] in it, and for
// each part the median and the 95th percentile by the nearest rank in microseconds (-1: not known
// in any frame: the render thread's time between frames in the first frame, clock -> latch
// without latch times, a screen time without a present fence).
struct frame_summary {
    int64_t span_ns;
    int32_t frames;
    int32_t median_us[FRAME_PARTS];
    int32_t p95_us[FRAME_PARTS];
};

// The split's state. No allocation: all of it is here. Not thread-safe: its owner locks it.
struct frame_split {
    int64_t since, last;   // the window's first and last frame (their acquires); 0: no window
    int64_t last_presented; // the frame before's present; 0: none
    int frames;
    int32_t us[FRAME_PARTS][FRAME_SAMPLES];
};

// Forgets the window and the frame before.
void frame_split_reset(struct frame_split *s);

// The app showed [frame] (its times known: acquired not 0). Returns 1 when the window before ended
// (this frame came [window_ns] or more after the window's first, or the window was full), its
// summary in [ended]; this frame then starts the next window.
int frame_split_shown(struct frame_split *s, const struct split_frame *frame, int64_t window_ns, struct frame_summary *ended);

#ifdef __cplusplus
}
#endif
