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

// The app's clock ticked or stopped (frame_clock.c): while a frame with vsync off waits for a tick,
// this wakes the server's thread, which shows it; nothing else on the clock's thread.
void present_server_tick(void);

#ifdef __cplusplus
}
#endif
