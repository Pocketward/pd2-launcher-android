// The rules of the app's clock (frame_clock.c) and of the direct present's newest frame and its
// count of the frames' time on the screen (present_server.cpp), apart from the Android calls so
// that the build host can test them (app/src/test/cpp).
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The present counter's page (one page of the file PD2_PRESENT_COUNTER names), as the game's Mesa
// maps it: the presents at byte 0 (patch 0002, Mesa's), the last present the Game view's copy has
// taken off the X screen at byte 64 and, at byte 128, non-zero while the copy runs (patch 0003,
// for the back pixmap of patch 0007), the direct present's word at byte 192 (patch 0004), the
// clock's ticks at byte 256 and, at byte 320, non-zero while the clock ticks (patch 0008); and for
// Detailed logging's pointer split (patch 0008, pointer_split.h), at byte 384 non-zero while the
// app wants the frames' times, Mesa's count of the frames it timed at byte 448, and the ring of
// their times from byte 512 (SPLIT_RING_BYTE).
#define PACING_PAGE 4096
#define TAKEN_WORD (64 / 4)
#define PACED_WORD (128 / 4)
#define TICK_WORD (256 / 4)
#define CLOCK_WORD (320 / 4)
#define STAMPS_WORD (384 / 4)

// The tick after one due at [due], [interval] nanoseconds on; one that would already be past at
// [now] (the app woken late, the device asleep) comes one interval after [now]: no burst of ticks.
int64_t clock_next(int64_t due, int64_t now, int64_t interval);

// Whether a frame may go to the screen now: the app's clock has ticked ([tick], the tick word)
// since the last frame shown, at [shown_tick] ([shown]: one was), or the clock does not run
// ([clock_running] 0). The screen shows at most one of the game's frames a tick, however fast the
// game draws (vsync off); a game is never held for it, the frames between go back or are dropped.
int tick_due(uint32_t tick, uint32_t shown_tick, int shown, int clock_running);

// One frame the game sent to the direct present: buffer [index] of a buffer set [set] (the
// present server's), with its acquire [fence] (-1: none) and the [serial] of its times in the
// present counter's page (pointer_split.h; 0: none).
struct sent_frame {
    void *set;
    int index;
    int fence;
    uint32_t serial;
};

// The newest frame not yet shown: the present server keeps one while a transaction is on its way
// to the screen, or while no Game view shows (for the return). A newer frame replaces it.
struct newest_frame {
    int kept;
    struct sent_frame frame;
};

// Keeps [frame] as the newest; returns 1 with the frame it replaced in [replaced] (it goes back to
// the game at once, unshown), else 0.
int keep_newest(struct newest_frame *n, struct sent_frame frame, struct sent_frame *replaced);

// Takes the newest frame out into [frame]; 0 when none is kept.
int take_newest(struct newest_frame *n, struct sent_frame *frame);

// The groups the direct present counts the game's frames in by their time on the screen: 1, 2, 3,
// 4, and 5 or more refreshes of the display.
#define ON_SCREEN_GROUPS 5

// How many refreshes of [refresh_ns] (the display's refresh period) a frame took that stayed
// [on_screen_ns] on the screen (from its present fence to the next frame's): the nearest whole
// number, at least 1 and at most ON_SCREEN_GROUPS (5 or more); 0 while the period is not known.
int refreshes_on_screen(int64_t on_screen_ns, int64_t refresh_ns);

#ifdef __cplusplus
}
#endif
