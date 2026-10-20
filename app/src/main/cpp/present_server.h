// The game's direct present (present_server.cpp): the app's side of Mesa patch 0004.
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Whether the Game view shows the game's own buffers now: 0 no (the X screen's copy shows the
// game), 1 over the whole view (the copy rests), 2 over the game's window only, a windowed game's
// (the copy keeps the X screen around it current).
int present_server_showing(void);

// A display frame at [frame_ns]: the Game view's copy calls it on each one. At a reduced frame rate
// the display frames that take one (frame_pacing.h) show the oldest buffer the game sent.
void present_server_vsync(int64_t frame_ns);

#ifdef __cplusplus
}
#endif
