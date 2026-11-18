/*
 * The clipboard between Android and the game, for x-input-relay (x-input-relay.c). Wine's X11
 * driver keeps the Windows clipboard in step with the X screen's CLIPBOARD selection, so:
 *   - Android to the game: the app sends Android's clipboard text (RELAY_CLIPBOARD); the relay
 *     owns CLIPBOARD with it, and Wine reads it when the game asks the Windows clipboard (BH.dll's
 *     right click in the chat box and Ctrl+V, PD2's Ctrl+V in its chat line). Wine asks for
 *     TARGETS when the owner changes and for UTF8_STRING only when the game reads the clipboard.
 *   - The game to Android: when another X client (Wine, after the game copied: BH's Ctrl+C on an
 *     item, PD2's guild link) takes CLIPBOARD, XFixes tells the relay; it asks the new owner for
 *     UTF8_STRING and tells the app the text ('T'), which puts it on Android's clipboard.
 * A change the relay made itself (it took CLIPBOARD) is not told back. Text only, at most
 * CLIPBOARD_MAX bytes each way; the log has lengths, never the text.
 */
#pragma once

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/extensions/Xfixes.h>

/* The most bytes of text each way (RELAY_CLIPBOARD's length, the 'T' word's). */
#define CLIPBOARD_MAX 65536

struct clipboard {
    Display *display;
    Window window; /* an unmapped window of the relay's own: owns CLIPBOARD and takes the game's text */
    Atom selection, targets, utf8, incr, property;
    int fixes_event; /* XFixes' first event code; -1: no XFixes, the game's copies stay in the game */
    unsigned char *text; /* the text the relay owns CLIPBOARD with, text_length bytes; NULL: none */
    size_t text_length;
};

static inline void clipboard_init(struct clipboard *c, Display *display, int fixes_event) {
    memset(c, 0, sizeof *c);
    c->display = display;
    c->fixes_event = fixes_event;
    c->window = XCreateSimpleWindow(display, DefaultRootWindow(display), -1, -1, 1, 1, 0, 0, 0);
    c->selection = XInternAtom(display, "CLIPBOARD", False);
    c->targets = XInternAtom(display, "TARGETS", False);
    c->utf8 = XInternAtom(display, "UTF8_STRING", False);
    c->incr = XInternAtom(display, "INCR", False);
    c->property = XInternAtom(display, "_PD2_CLIPBOARD", False);
    if (fixes_event >= 0) XFixesSelectSelectionInput(display, c->window, c->selection, XFixesSetSelectionOwnerNotifyMask);
}

/* Android's clipboard text, [length] bytes of UTF-8, for the game: the relay owns CLIPBOARD with it. */
static inline void clipboard_offer(struct clipboard *c, const unsigned char *bytes, size_t length) {
    unsigned char *copy = malloc(length ? length : 1);
    if (copy == NULL) {
        fprintf(stderr, "x-input-relay: clipboard: no memory for %zu bytes\n", length);
        return;
    }
    memcpy(copy, bytes, length);
    free(c->text);
    c->text = copy;
    c->text_length = length;
    XSetSelectionOwner(c->display, c->selection, c->window, CurrentTime);
    if (XGetSelectionOwner(c->display, c->selection) == c->window) {
        fprintf(stderr, "x-input-relay: clipboard: %zu bytes from Android for the game\n", length);
    } else {
        fprintf(stderr, "x-input-relay: clipboard: the X screen refused the relay CLIPBOARD\n");
    }
}

/* Answers [request] for the text the relay owns: TARGETS, UTF8_STRING; anything else is refused. */
static inline void clipboard_answer(struct clipboard *c, const XSelectionRequestEvent *request) {
    /* A client from before ICCCM 2 names no property: the target is the property then. */
    Atom property = request->property != None ? request->property : request->target;
    if (request->target == c->targets) {
        Atom list[2] = {c->targets, c->utf8};
        XChangeProperty(c->display, request->requestor, property, XA_ATOM, 32, PropModeReplace, (unsigned char *)list, 2);
    } else if (request->target == c->utf8 && c->text != NULL) {
        XChangeProperty(c->display, request->requestor, property, c->utf8, 8, PropModeReplace, c->text, (int)c->text_length);
    } else {
        property = None;
    }
    XEvent reply;
    memset(&reply, 0, sizeof reply);
    reply.xselection.type = SelectionNotify;
    reply.xselection.display = c->display;
    reply.xselection.requestor = request->requestor;
    reply.xselection.selection = request->selection;
    reply.xselection.target = request->target;
    reply.xselection.property = property;
    reply.xselection.time = request->time;
    XSendEvent(c->display, request->requestor, False, NoEventMask, &reply);
}

/* The new owner's text arrived ([notify]): [tell] gets it, unless it is refused, too large or empty. */
static inline void clipboard_take(struct clipboard *c, const XSelectionEvent *notify,
                                  void (*tell)(const unsigned char *bytes, size_t length)) {
    if (notify->property == None) {
        fprintf(stderr, "x-input-relay: clipboard: the game's copy holds no text\n");
        return;
    }
    Atom type;
    int format;
    unsigned long items, after;
    unsigned char *data = NULL;
    if (XGetWindowProperty(c->display, c->window, notify->property, 0, CLIPBOARD_MAX / 4 + 1, True, AnyPropertyType,
                           &type, &format, &items, &after, &data) != Success) {
        fprintf(stderr, "x-input-relay: clipboard: cannot read the game's copy\n");
        return;
    }
    if (type == c->incr || after > 0 || (format == 8 && items > CLIPBOARD_MAX)) {
        fprintf(stderr, "x-input-relay: clipboard: the game's copy is over %d bytes: not for Android\n", CLIPBOARD_MAX);
    } else if (type != c->utf8 || format != 8) {
        fprintf(stderr, "x-input-relay: clipboard: the game's copy is not UTF-8 text\n");
    } else if (items > 0) {
        fprintf(stderr, "x-input-relay: clipboard: %lu bytes from the game for Android\n", items);
        tell(data, items);
    }
    if (data != NULL) XFree(data);
}

/* [event] when it is the clipboard's, with 1; else 0 (the caller's event). [tell]: the game's text for the app. */
static inline int clipboard_event(struct clipboard *c, XEvent *event, void (*tell)(const unsigned char *bytes, size_t length)) {
    switch (event->type) {
    case SelectionRequest:
        if (event->xselectionrequest.owner != c->window) return 0;
        clipboard_answer(c, &event->xselectionrequest);
        return 1;
    case SelectionClear:
        if (event->xselectionclear.window != c->window) return 0;
        /* Another client took CLIPBOARD: XFixes' notice of it fetches its text. */
        free(c->text);
        c->text = NULL;
        c->text_length = 0;
        return 1;
    case SelectionNotify:
        if (event->xselection.requestor != c->window) return 0;
        clipboard_take(c, &event->xselection, tell);
        return 1;
    default:
        if (c->fixes_event < 0 || event->type != c->fixes_event + XFixesSelectionNotify) return 0;
        {
            XFixesSelectionNotifyEvent *notify = (XFixesSelectionNotifyEvent *)event;
            if (notify->selection == c->selection && notify->owner != None && notify->owner != c->window) {
                XConvertSelection(c->display, c->selection, c->utf8, c->property, c->window, notify->selection_timestamp);
            }
        }
        return 1;
    }
}
