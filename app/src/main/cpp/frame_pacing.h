// The Game view's rule for a reduced frame rate (frame_pacing.c), apart from the Android calls, so
// that the build host can test it (app/src/test/cpp).
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// A display frame this much before a take is due still takes: the display frames' times wobble a
// little, and a take is never left for the display frame after.
#define TAKE_EARLY_NS 2000000LL

// Which display frames take the game's next frame at a reduced rate: on the copy path the copy
// takes it off the X screen (frame_copy.c), on the direct present the app shows the buffer the game
// sent (present_server.cpp). Where the interval is a whole number of the display's frames (within
// 2 %: 30 fps on 60, 90 or 120 Hz, 60 on 120 Hz), every so many display frames take, evenly, by the
// display's measured period, so a display a little off its nominal rate never takes one display
// frame early or late. Otherwise the due times are one interval apart and the first display frame
// at or after each takes, so the average is the reduced rate (60 on a 90 Hz panel: two display
// frames of three, unevenly).
struct take_schedule {
    int64_t interval_ns; // 0: every display frame takes
    int64_t next_ns;     // the next due time; 0 before the first take
    int64_t last_ns;     // the last display frame that took; 0 none
    int64_t period_ns;   // the display's frame period, from the gaps between display frames; 0 unknown
    int64_t frame_ns;    // the last display frame seen; 0 none
    int outliers;        // display frames in a row whose gap did not fit the period
};

// A display frame came at [frame_ns]: call it for every display frame, before take_due. The
// display's period follows the gaps between display frames (a missed display frame's double gap is
// left out; a new rate, seen 8 display frames in a row, is measured again).
void take_display(struct take_schedule *s, int64_t frame_ns);

// Whether the display frame at [frame_ns] takes a frame. One that takes moves the due times on by
// an interval, or, more than an interval late (no display frames, or no frame to take, for a
// while), starts them again one interval after this display frame. [pending]: a changed picture
// waits for a free buffer; it is taken on this display frame, and the due times stay.
int take_due(struct take_schedule *s, int64_t frame_ns, int pending);

#ifdef __cplusplus
}
#endif
