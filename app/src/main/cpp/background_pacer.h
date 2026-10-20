// The game's pacing while the Game view does not pace it (background_pacer.c).
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// A Game view copy starts to pace the game through [page], its own mapping of the present
// counter's page (NULL: it has none): from now on Mesa waits for the copy's takes.
void pacing_copy_started(uint32_t *page);

// A Game view copy stops pacing the game: with no copy left, the background pacer holds the game
// from here when it runs, else Mesa waits for no one (told through [page]; NULL: nothing to tell).
void pacing_copy_stopped(uint32_t *page);

// The interval the background pacer holds the game to while it runs, in nanoseconds; 0 while it
// does not. Without a Game view the direct present holds the game's frames to it (else they go to
// the X screen).
int64_t pacing_background_interval(void);

#ifdef __cplusplus
}
#endif
