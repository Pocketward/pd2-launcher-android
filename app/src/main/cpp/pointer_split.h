// Detailed logging's split of the pointer's delay after the app's write of a move into
// x-input-relay's socket, apart from the Android calls so that the build host can test it
// (app/src/test/cpp/pointer_split_test.c). The direct present (present_server.cpp) keeps one.
//
// The times, all CLOCK_MONOTONIC (System.nanoTime's clock in the app, clock_gettime in Mesa):
// - the write (T_in): the Game view's write of a pointer move into the relay's socket ends
//   (GameView.send, the time that MoveDelays also takes; split_move);
// - acquired (T_acq): the game's acquire gives it a swapchain image (Mesa patch 0008,
//   x11_acquire_next_image): the first acquire after a write starts the first frame that can show
//   that move;
// - presented (T_pres): the game's vkQueuePresentKHR queues that image (x11_queue_present; with
//   Zink, on its submit thread after the game's SwapBuffers);
// - GPU done: Mesa's present thread ends its wait for the image's fence (patch 0006): the GPU has
//   done the frame and its blit into the app's buffer;
// - clock (T_tick): Mesa's present thread ends its wait for the app's clock (patch 0008,
//   sw_present_clock), and the frame goes to the app;
// - applied (T_apply): the app's SurfaceControl transaction that shows the frame
//   (present_server.cpp, show: just before ASurfaceTransaction_apply);
// - latched (T_latch): the compositor latched that transaction (ASurfaceTransactionStats_
//   getLatchTime in its OnComplete: SurfaceFlinger's own time, not the callback's arrival); the
//   frame shows at the display's next vsync after it. Clock -> latch is the phase of our clock's
//   tick against the display's latch deadline: how long a frame that is ready waits for Android;
//   a frame that is ready just after a latch waits a whole display period;
// - screen (T_screen): that transaction's present fence signals (its frame is on the screen), from
//   the fence's signal time (never the callback's arrival).
// Mesa writes its four times into a ring in the present counter's page (below) and sends the
// frame's serial with its PRESENT; the app reads them when it shows the frame.
//
// Each move counts once, with the first frame the app shows whose acquire came at or after its
// write (a frame that the app replaced unshown passes its moves on to the next one shown). Mesa's
// acquire and present come from D2GL's render thread, not from the game's thread that reads the
// pointer: D2GL (PD2's fork, 55148de0, graphic/context.cpp) has a ring of frame_latency + 1
// command slots (2 by default); the game's thread fills one while the render thread draws the
// other, and the render thread frees its slot before SwapBuffers. So the pointer in a frame can be
// older than its acquire, and the move then shows one frame (or more) later than write -> screen
// says; a game that reads the pointer after its acquire can show a move written after it. Write ->
// acquire is the wait for the next frame start, not a proof of which frame shows the move. The
// other parts are the frame's own times.
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The ring of Mesa's frame times in the present counter's page (frame_pacing.h): SPLIT_SLOTS
// slots of SPLIT_SLOT_BYTES from byte SPLIT_RING_BYTE. Slot [serial % SPLIT_SLOTS] holds frame
// [serial] (never 0): its serial (32 bits; 0 while Mesa writes the slot), 4 bytes not used, then
// the frame's times (64 bits each): acquired, presented, GPU done, clock. Mesa writes the slot as
// a sequence lock: 0 into the serial, the times, then the serial.
#define SPLIT_RING_BYTE 512
#define SPLIT_SLOT_BYTES 64
#define SPLIT_SLOTS 16

// One frame's times in nanoseconds (0: not known).
struct split_frame {
    int64_t acquired;
    int64_t presented;
    int64_t gpu_done;
    int64_t clock;
    int64_t applied;
    int64_t latched;
    int64_t screen;
};

// Mesa's times of frame [serial] in [page] into [frame] (acquired to clock); 0 when there are none:
// no page, serial 0, or a slot that holds no whole copy of that frame (Mesa writes it now, or a
// later frame has the slot).
int split_read(const uint32_t *page, uint32_t serial, struct split_frame *frame);

// The parts of a move's delay, in the summary's order.
enum split_part {
    SPLIT_WRITE_ACQUIRE,   // the write to the next acquire: the wait for the next frame start
    SPLIT_ACQUIRE_PRESENT, // the acquire to the present: the game's frame
    SPLIT_PRESENT_CLOCK,   // the present to the clock: the GPU's end of the frame, then our pacing
    SPLIT_PRESENT_GPU,     // of that, the present to GPU done
    SPLIT_CLOCK_APPLY,     // the clock to the transaction: the app
    SPLIT_APPLY_SCREEN,    // the transaction to the screen: SurfaceFlinger and the display
    SPLIT_CLOCK_LATCH,     // the clock to the latch: our tick's phase against the display's latch
    SPLIT_WRITE_SCREEN,    // the whole: the write to the screen
    SPLIT_PARTS,
};

// A summary ends at the first frame shown this long after its window's first, or when it holds
// SPLIT_SAMPLES moves.
#define SPLIT_WINDOW_NS 5000000000LL
#define SPLIT_SAMPLES 2048
// The moves that wait for their frame; with more, the oldest is not counted.
#define SPLIT_MOVES 512
// A move written this long before the acquire of its frame is not counted (no frame of the
// direct present came in that time: the X screen showed the game, or it drew none).
#define SPLIT_OLDEST_NS 500000000LL

// One window's summary: from the first frame shown that took a move to the last ([span_ns]), the
// moves counted and the frames that showed them; the moves not counted (with no time on the
// screen, too old, or past SPLIT_SAMPLES or SPLIT_MOVES); for each part, the median and the 95th
// percentile by the nearest rank, in microseconds (0 without moves; -1 for clock -> latch when no
// frame had a latch time).
struct split_summary {
    int64_t span_ns;
    int32_t moves;
    int32_t frames;
    int32_t not_counted;
    int32_t median_us[SPLIT_PARTS];
    int32_t p95_us[SPLIT_PARTS];
};

// The split's state: the moves that wait for their frame (oldest first) and the window's samples.
// No allocation: all of it is here. Not thread-safe: its owner locks it.
struct pointer_split {
    int64_t moves[SPLIT_MOVES];
    int first, count;
    int64_t since, last; // the window's first and last frame shown that took a move; 0: no window
    int samples, frames, not_counted;
    int32_t us[SPLIT_PARTS][SPLIT_SAMPLES];
};

// Forgets every move and the window.
void split_reset(struct pointer_split *s);

// A pointer move was written into the relay's socket at [written].
void split_move(struct pointer_split *s, int64_t written);

// The app showed [frame] (its times known; latched 0: no latch time; screen 0: no present fence
// time): the moves written
// at or before its acquire go with it. Returns 1 when the window before ended, its summary in
// [ended].
int split_shown(struct pointer_split *s, const struct split_frame *frame, struct split_summary *ended);

#ifdef __cplusplus
}
#endif
