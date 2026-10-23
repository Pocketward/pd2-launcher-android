// The app's clock (frame_clock.c): the Game view copies' part (frame_copy.c).
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// A Game view copy starts to read the X screen through [page], its own mapping of the present
// counter's page (NULL: it has none): Mesa writes the X screen's frames into its back pixmap and
// waits for the copy's takes before it copies them into the window (patches 0003 and 0007).
void pacing_copy_started(uint32_t *page);

// A Game view copy stops: with no copy left, Mesa writes the X screen's frames into the window at
// once (told through [page]; NULL: nothing to tell).
void pacing_copy_stopped(uint32_t *page);

#ifdef __cplusplus
}
#endif
