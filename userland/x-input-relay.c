/*
 * x-input-relay: the app's touch and keyboard input into the X screen. The app's service starts
 * it with Xvfb, DISPLAY naming that screen, and its end of a socket pair as standard input. It
 * reads fixed-size events from that socket and injects each into the X screen through XTEST, so
 * the game sees an ordinary X pointer and keyboard. It ends when the app closes its end (exit 0),
 * or when the X screen goes away.
 *
 * It also watches the X screen's windows and tells the app, on the same socket, each time the
 * answer changes, whether the game's frames may be shown straight from its own buffers
 * (userland/patches/mesa/0004). The game's window is the one Mesa names in the root window's
 * _PD2_PRESENT_WINDOW property once the game's swapchain is set up for the app:
 *   'F'  the game's window alone: it is mapped and on the screen, no window over it shows, and
 *        none beside it (a windowed game's window does not cover the screen: Wine places it as
 *        if a window manager framed it). Then six little-endian i32: the window's x, y, width
 *        and height on the X screen (parts may lie off it), and the X screen's width and height.
 *   'C'  a window shows over or beside it (a Wine dialog, the official launcher), it is not
 *        mapped, off the screen or partly hidden by a window it is in, or no window is named
 * Windows one pixel wide or high (Wine's 1x1 helpers) and windows inside the game's own count as
 * nothing.
 *
 * This file is the one place both formats are defined; the app's encoder and decoder
 * (app/src/main/java/com/pd2launcher/app/input/RelayEvents.kt) mirror them and its unit test
 * pins the bytes.
 *
 *   struct relay_event, 12 bytes, little endian (both ends run on the same device):
 *     0 u32 type    4 i32 a    8 i32 b
 *
 *   RELAY_MOTION   a, b = x, y        absolute pointer position on the X screen
 *   RELAY_RELATIVE a, b = dx, dy      relative pointer motion
 *   RELAY_BUTTON   a = X button (1 left, 2 middle, 3 right, 4/5 wheel up/down), b = 1 down, 0 up
 *   RELAY_KEY      a = X keysym, b = 1 down, 0 up
 *
 * A keysym on no key of the current keymap (text from Android's keyboard, such as an accented
 * letter) is put on a spare keycode first; a keysym on a key's shifted level is sent with Shift.
 *
 * Exit codes: 0 the app closed the socket; 1 bad usage, a broken event stream, or the X screen
 * went away (Xlib's I/O error handler); 2 no X screen; 3 the X screen has no XTEST.
 */
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <X11/XKBlib.h>
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/extensions/XTest.h>
#include <X11/keysym.h>

struct relay_event {
    uint32_t type;
    int32_t a;
    int32_t b;
};
_Static_assert(sizeof(struct relay_event) == 12, "the relay event is 12 bytes");

enum {
    RELAY_MOTION = 1,
    RELAY_RELATIVE = 2,
    RELAY_BUTTON = 3,
    RELAY_KEY = 4,
};

static Display *display;
static KeyCode spare; /* a keycode with no keysyms, for keysyms on no key; 0 if none */
static Atom present_window; /* _PD2_PRESENT_WINDOW */
/* What was last told: 'C', or 'F' and where; told[0] 0 before the first word. */
static int32_t told[7];

/* Reads one whole event; 0 at end of stream, -1 on error or a partial event. */
static int read_event(struct relay_event *event) {
    size_t have = 0;
    while (have < sizeof *event) {
        ssize_t n = read(0, (char *)event + have, sizeof *event - have);
        if (n == 0) return have == 0 ? 0 : -1;
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        have += (size_t)n;
    }
    return 1;
}

/* The highest keycode that carries no keysym, or 0. */
static KeyCode find_spare(void) {
    int min, max, per;
    XDisplayKeycodes(display, &min, &max);
    KeySym *map = XGetKeyboardMapping(display, (KeyCode)min, max - min + 1, &per);
    if (map == NULL) return 0;
    KeyCode found = 0;
    for (int code = max; code >= min && !found; code--) {
        int empty = 1;
        for (int i = 0; i < per; i++) {
            if (map[(code - min) * per + i] != NoSymbol) empty = 0;
        }
        if (empty) found = (KeyCode)code;
    }
    XFree(map);
    return found;
}

static void key(KeySym sym, int down) {
    KeyCode code = XKeysymToKeycode(display, sym);
    int shift = 0;
    if (code != 0) {
        shift = XkbKeycodeToKeysym(display, code, 0, 0) != sym &&
                XkbKeycodeToKeysym(display, code, 0, 1) == sym;
    } else if (spare != 0) {
        KeySym both[2] = {sym, sym};
        XChangeKeyboardMapping(display, spare, 2, both, 1);
        XSync(display, False);
        code = spare;
    } else {
        fprintf(stderr, "x-input-relay: keysym 0x%lx is on no key and no keycode is spare\n", sym);
        return;
    }
    KeyCode shift_code = shift ? XKeysymToKeycode(display, XK_Shift_L) : 0;
    if (shift_code && down) XTestFakeKeyEvent(display, shift_code, True, CurrentTime);
    XTestFakeKeyEvent(display, code, down ? True : False, CurrentTime);
    if (shift_code && down) XTestFakeKeyEvent(display, shift_code, False, CurrentTime);
}

/* A window can go between listing it and reading its attributes: that BadWindow is no error. */
static int x_error(Display *d, XErrorEvent *e) {
    (void)d;
    if (e->error_code != BadWindow && e->error_code != BadDrawable) {
        fprintf(stderr, "x-input-relay: X error %d (request %d)\n", e->error_code, e->request_code);
    }
    return 0;
}

/* The window Mesa named in the root window's _PD2_PRESENT_WINDOW property, or None. */
static Window named_window(void) {
    Atom type = None;
    int format = 0;
    unsigned long count = 0, after = 0;
    unsigned char *data = NULL;
    Window found = None;
    if (XGetWindowProperty(display, DefaultRootWindow(display), present_window, 0, 1, False, XA_WINDOW, &type,
                           &format, &count, &after, &data) == Success &&
        type == XA_WINDOW && format == 32 && count == 1 && data != NULL) {
        found = (Window)((unsigned long *)data)[0]; /* Xlib hands 32-bit items over as longs */
    }
    if (data != NULL) XFree(data);
    return found;
}

struct box {
    int left, top, right, bottom;
};

/* The part of a [w] x [h] window at [x], [y] on the [width] x [height] X screen. */
static struct box on_screen(int x, int y, int w, int h, int width, int height) {
    struct box b = {x < 0 ? 0 : x, y < 0 ? 0 : y, x + w > width ? width : x + w, y + h > height ? height : y + h};
    return b;
}

static int empty(struct box b) {
    return b.right <= b.left || b.bottom <= b.top;
}

/* Whether [a] lies within [b]. */
static int inside(struct box a, struct box b) {
    return a.left >= b.left && a.top >= b.top && a.right <= b.right && a.bottom <= b.bottom;
}

/* Whether [a] and [b] overlap. */
static int overlap(struct box a, struct box b) {
    return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
}

/* The part on the screen of a viewable InputOutput window more than one pixel wide and high
 * (Wine's helpers are 1x1); an empty box for any other. */
static struct box shows(Window window, int width, int height) {
    XWindowAttributes a;
    int x, y;
    Window child;
    struct box none = {0, 0, 0, 0};
    if (!XGetWindowAttributes(display, window, &a) || a.map_state != IsViewable || a.class != InputOutput || a.width <= 1 ||
        a.height <= 1 || !XTranslateCoordinates(display, window, DefaultRootWindow(display), 0, 0, &x, &y, &child)) {
        return none;
    }
    return on_screen(x - a.border_width, y - a.border_width, a.width + 2 * a.border_width, a.height + 2 * a.border_width, width,
                     height);
}

/* [window] and the windows it is in, up to the root window's child: their moves, resizes, maps
 * and children, which no root listener hears of, come to the relay too. */
static void follow(Window window) {
    Window root = DefaultRootWindow(display);
    while (window != None && window != root) {
        XSelectInput(display, window, StructureNotifyMask | SubstructureNotifyMask);
        Window root_return, parent, *children = NULL;
        unsigned int count = 0;
        if (!XQueryTree(display, window, &root_return, &parent, &children, &count)) return;
        if (children != NULL) XFree(children);
        window = parent;
    }
}

/* [window]'s ancestor among the root window's children (itself when it is one); None when a
 * window it is in cuts off part of [box], the window's part of the screen (Wine parks a GL window
 * it hides in a 1x1 window at -1,-1: mapped, but nothing of it shows), or a window above it in one
 * of them shows over it. */
static Window top_level(Window window, struct box box, int width, int height) {
    Window root = DefaultRootWindow(display);
    while (window != None && window != root) {
        Window root_return, parent, *children = NULL;
        unsigned int count = 0;
        if (!XQueryTree(display, window, &root_return, &parent, &children, &count)) return None;
        if (children != NULL) XFree(children);
        if (parent == root) return window;
        XWindowAttributes a;
        int x, y;
        Window child;
        if (!XGetWindowAttributes(display, parent, &a) || !XTranslateCoordinates(display, parent, root, 0, 0, &x, &y, &child) ||
            !inside(box, on_screen(x, y, a.width, a.height, width, height))) {
            return None;
        }
        /* Its siblings above it, bottom to top after it. */
        if (!XQueryTree(display, parent, &root_return, &child, &children, &count)) return None;
        int above = 0, covered = 0;
        for (unsigned int i = 0; i < count && !covered; i++) {
            if (children[i] == window) above = 1;
            else if (above && overlap(shows(children[i], width, height), box)) covered = 1;
        }
        if (children != NULL) XFree(children);
        if (covered) return None;
        window = parent;
    }
    return None;
}

/* Whether the game's window, in the top-level window [top], is all the X screen shows but the
 * root window's background: [box] is the part of it on the screen. */
static int alone(Window top, struct box box, int width, int height) {
    Window root_return, parent, *children = NULL;
    unsigned int count = 0;
    if (!XQueryTree(display, DefaultRootWindow(display), &root_return, &parent, &children, &count)) return 0;
    int over = 0, result = 1;
    /* Bottom to top. */
    for (unsigned int i = 0; i < count && result; i++) {
        XWindowAttributes a;
        if (children[i] == top) {
            /* Its own top-level window shows nothing beside the game's (a frame Wine drew itself). */
            if (!XGetWindowAttributes(display, top, &a) ||
                !inside(on_screen(a.x, a.y, a.width + 2 * a.border_width, a.height + 2 * a.border_width, width, height), box)) {
                result = 0;
            }
            over = 1;
            continue;
        }
        struct box b = shows(children[i], width, height);
        if (empty(b)) continue;
        /* Over the game's window it shows; under it, wherever the game's window does not hide it. */
        if (over || !inside(b, box)) result = 0;
    }
    if (children != NULL) XFree(children);
    return result && over;
}

/* Tells the app 'C', or 'F' and where (above), when it changed. */
static void tell_cover(void) {
    Window root = DefaultRootWindow(display);
    int width = DisplayWidth(display, DefaultScreen(display));
    int height = DisplayHeight(display, DefaultScreen(display));
    int32_t word[7] = {'C', 0, 0, 0, 0, 0, 0};
    Window game = named_window();
    follow(game);
    XWindowAttributes g;
    int x, y;
    Window child, top = None;
    if (game != None && XGetWindowAttributes(display, game, &g) && g.map_state == IsViewable &&
        XTranslateCoordinates(display, game, root, 0, 0, &x, &y, &child)) {
        struct box box = on_screen(x, y, g.width, g.height, width, height);
        if (!empty(box) && (top = top_level(game, box, width, height)) != None && alone(top, box, width, height)) {
            int32_t where[7] = {'F', x, y, g.width, g.height, width, height};
            memcpy(word, where, sizeof word);
        }
    }
    if (memcmp(word, told, sizeof word) == 0) return;
    memcpy(told, word, sizeof word);
    /* 'C', or 'F' and its six numbers (both ends run on the same device: its byte order). */
    unsigned char bytes[1 + 6 * sizeof(int32_t)];
    bytes[0] = (unsigned char)word[0];
    memcpy(bytes + 1, word + 1, 6 * sizeof(int32_t));
    size_t length = word[0] == 'F' ? sizeof bytes : 1, sent = 0;
    while (sent < length) {
        ssize_t n = write(0, bytes + sent, length - sent);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) {
            fprintf(stderr, "x-input-relay: cannot tell the app the windows' state\n");
            break;
        }
        sent += (size_t)n;
    }
    if (word[0] == 'F') {
        fprintf(stderr, "x-input-relay: the game's window alone: %dx%d at %d,%d on the %dx%d X screen\n", word[3], word[4],
                word[1], word[2], word[5], word[6]);
    } else {
        fprintf(stderr, "x-input-relay: not the game's window alone (another window shows, or none is named or mapped)\n");
    }
}

static void inject(const struct relay_event *event) {
    switch (event->type) {
    case RELAY_MOTION:
        XTestFakeMotionEvent(display, -1, event->a, event->b, CurrentTime);
        break;
    case RELAY_RELATIVE:
        XTestFakeRelativeMotionEvent(display, event->a, event->b, CurrentTime);
        break;
    case RELAY_BUTTON:
        if (event->a >= 1 && event->a <= 9) {
            XTestFakeButtonEvent(display, (unsigned)event->a, event->b ? True : False, CurrentTime);
        }
        break;
    case RELAY_KEY:
        key((KeySym)(uint32_t)event->a, event->b);
        break;
    default:
        fprintf(stderr, "x-input-relay: unknown event type %u\n", event->type);
        break;
    }
    XFlush(display);
}

int main(int argc, char **argv) {
    (void)argv;
    if (argc != 1) {
        fprintf(stderr, "usage: x-input-relay < socket (DISPLAY names the X screen)\n");
        return 1;
    }
    display = XOpenDisplay(NULL);
    if (display == NULL) {
        fprintf(stderr, "x-input-relay: cannot open the X screen\n");
        return 2;
    }
    int event_base, error_base, major, minor;
    if (!XTestQueryExtension(display, &event_base, &error_base, &major, &minor)) {
        fprintf(stderr, "x-input-relay: the X screen has no XTEST\n");
        return 3;
    }
    spare = find_spare();
    fprintf(stderr, "x-input-relay: XTEST %d.%d, spare keycode %d\n", major, minor, spare);
    XSetErrorHandler(x_error);
    present_window = XInternAtom(display, "_PD2_PRESENT_WINDOW", False);
    /* The top-level windows coming, going, moving and restacking, and Mesa naming the game's. */
    XSelectInput(display, DefaultRootWindow(display), SubstructureNotifyMask | PropertyChangeMask);
    tell_cover();
    XFlush(display);

    struct relay_event event;
    int got = 1;
    while (got > 0) {
        if (XPending(display)) {
            while (XPending(display)) {
                XEvent x;
                XNextEvent(display, &x);
            }
            tell_cover();
        }
        XFlush(display);
        /* Input is read every round; events read while answering are looked at next round. */
        int queued = XEventsQueued(display, QueuedAlready);
        struct pollfd fds[2] = {{0, POLLIN, 0}, {ConnectionNumber(display), POLLIN, 0}};
        if (poll(fds, 2, queued > 0 ? 0 : -1) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (fds[0].revents & (POLLIN | POLLHUP | POLLERR)) {
            got = read_event(&event);
            if (got > 0) inject(&event);
        }
    }
    XCloseDisplay(display);
    if (got < 0) {
        fprintf(stderr, "x-input-relay: broken event stream\n");
        return 1;
    }
    return 0;
}
