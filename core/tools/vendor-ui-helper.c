typedef void *HWND;
typedef unsigned short WCHAR;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef long LPARAM;
typedef unsigned long WPARAM;

__declspec(dllimport) HWND __stdcall FindWindowW(const WCHAR *, const WCHAR *);
__declspec(dllimport) HWND __stdcall FindWindowExW(HWND, HWND, const WCHAR *, const WCHAR *);
__declspec(dllimport) int __stdcall GetWindowTextW(HWND, WCHAR *, int);
__declspec(dllimport) int __stdcall IsWindowEnabled(HWND);
__declspec(dllimport) int __stdcall IsWindowVisible(HWND);
__declspec(dllimport) int __stdcall PostMessageW(HWND, UINT, WPARAM, LPARAM);
__declspec(dllimport) void __stdcall Sleep(DWORD);
__declspec(dllimport) void __stdcall ExitProcess(UINT);

static const WCHAR BASE_TITLE[] = L"Diablo II Installer";
static const WCHAR EXPANSION_TITLE[] = L"Diablo II - Lord of Destruction";
static const WCHAR EXPANSION_INSTALLER_TITLE[] = L"Diablo II - Lord of Destruction Installer";
static const WCHAR EXPANSION_COLON_TITLE[] = L"Diablo II: Lord of Destruction Installer";
static const WCHAR EULA_TITLE[] = L"End User License Agreement";
static const WCHAR DESTINATION_TITLE[] = L"Installation Destination Directory";
static const WCHAR ERROR_TITLE[] = L"Installer";
static const WCHAR AGREE_TEXT[] = L"Agree";
static const WCHAR OK_TEXT[] = L"OK";

static int equal_button_text(const WCHAR *left, const WCHAR *right) {
    while (*left == L'&') left++;
    while (*right == L'&') right++;
    while (*left && *right) {
        if (*left != *right) return 0;
        left++;
        right++;
        while (*left == L'&') left++;
        while (*right == L'&') right++;
    }
    return *left == *right;
}

static int starts_with(const WCHAR *text, const WCHAR *prefix) {
    while (*prefix) {
        if (*text++ != *prefix++) return 0;
    }
    return 1;
}

/*
 * The installer rejects a wrong product key with "The information provided
 * was not correct." (the key is the only information it is given besides a
 * fixed user name); a text naming a key is treated the same.
 */
static int mentions_key(const WCHAR *text) {
    for (; *text; text++) {
        if ((text[0] == L'k' || text[0] == L'K') && (text[1] == L'e' || text[1] == L'E') &&
            (text[2] == L'y' || text[2] == L'Y'))
            return 1;
        if (starts_with(text, L"was not correct")) return 1;
    }
    return 0;
}

/*
 * Whether the generic error dialog's own text (its title excluded) names a
 * key. The text never leaves this process: only the exit status says which
 * kind of error dialog was seen.
 */
static int dialog_mentions_key(HWND parent, int depth) {
    if (depth <= 0) return 0;
    HWND previous = 0;
    for (;;) {
        HWND child = FindWindowExW(parent, previous, 0, 0);
        if (!child) return 0;
        previous = child;
        /* Not zero-initialized: that would need the C runtime's memset. */
        WCHAR observed[512];
        observed[0] = 0;
        observed[511] = 0;
        GetWindowTextW(child, observed, 511);
        if (mentions_key(observed)) return 1;
        if (dialog_mentions_key(child, depth - 1)) return 1;
    }
}

/*
 * A generic `Installer` error dialog aborts the stage: exit 22 when its text
 * says the product key was rejected, and 20 for any other error.
 */
static void exit_on_error_dialog(void) {
    HWND error = FindWindowW(0, ERROR_TITLE);
    if (!error) return;
    ExitProcess(dialog_mentions_key(error, 4) ? 22 : 20);
}

static HWND find_button(HWND parent, const WCHAR *text, int depth) {
    if (depth <= 0) return 0;
    HWND previous = 0;
    for (;;) {
        HWND child = FindWindowExW(parent, previous, 0, 0);
        if (!child) return 0;
        previous = child;
        /* Not zero-initialized: that would need the C runtime's memset. */
        WCHAR observed[64];
        observed[0] = 0;
        observed[63] = 0;
        GetWindowTextW(child, observed, 63);
        if (equal_button_text(observed, text)) return child;
        HWND nested = find_button(child, text, depth - 1);
        if (nested) return nested;
    }
}

/*
 * The helper outlives the installer's launching process and must exit on its
 * own so the Launcher's `wineserver -w` can prove the prefix quiet. Once any
 * reviewed installer window has been seen, a stretch without one means the
 * installer finished (a product whose consent or destination was already
 * settled shows neither dialog); the Auditor, not this exit, decides whether
 * it succeeded. Before the license dialog was answered that stretch is a
 * minute, because the installer's own window may close well before the
 * license dialog opens; afterwards it is five seconds. A helper that never
 * sees a reviewed window within ten minutes exits 21, so an installer that
 * ended without a window fails its stage instead of waiting for the
 * Launcher's two-hour stage bound.
 */
static HWND main_window(void) {
    HWND main = FindWindowW(0, BASE_TITLE);
    if (!main) main = FindWindowW(0, EXPANSION_TITLE);
    if (!main) main = FindWindowW(0, EXPANSION_INSTALLER_TITLE);
    if (!main) main = FindWindowW(0, EXPANSION_COLON_TITLE);
    return main;
}

static int installer_window_present(void) {
    return main_window() || FindWindowW(0, EULA_TITLE) || FindWindowW(0, DESTINATION_TITLE);
}

#define BEFORE_LICENSE_ABSENT_TICKS 600
#define AFTER_LICENSE_ABSENT_TICKS 50
#define NEVER_SEEN_TICKS 6000

static int seen_installer = 0;
static int absent_ticks = 0;
static int unseen_ticks = 0;

static void exit_when_installer_finished(int absent_limit) {
    if (installer_window_present()) {
        seen_installer = 1;
        absent_ticks = 0;
    } else if (seen_installer) {
        if (++absent_ticks >= absent_limit) ExitProcess(0);
    } else if (++unseen_ticks >= NEVER_SEEN_TICKS) {
        ExitProcess(21);
    }
}

/*
 * Blizzard's installer always opens its main menu window ("Install Diablo
 * II", "View the manual", "Customer support", "Exit installer"); with the
 * Launcher's silent options it opens the license dialog over it at once, and
 * the menu stays disabled until the installation itself runs. The menu is
 * left usable, with no dialog over it, when the license or destination
 * dialog was declined (Disagree, the license dialog's default button, or
 * Escape): the installer then waits there for a person. Before the license
 * was answered here, a menu that stays usable for two seconds is clicked on
 * its "Install" entry, which opens the license dialog again; a click the
 * menu ignores (as it does right after it appears) is retried, three times
 * in all. A menu still waiting a minute after the last click exits 23,
 * so the stage fails with its own result instead of waiting for the
 * Launcher's two-hour stage bound. After the license was answered the
 * installation runs in the same usable window with nothing to tell it from
 * the menu, so it is never clicked then; a window that stays usable with no
 * dialog for ten minutes (far longer than any installation) exits 23 too.
 */
#define MENU_SETTLE_TICKS 20
#define MENU_CLICKS 3
#define MENU_LAST_CLICK_TICKS 600
#define MENU_AFTER_LICENSE_TICKS 6000
#define MENU_EXIT 23

static int menu_ticks = 0;
static int menu_clicks = 0;

static void answer_menu(int license_answered) {
    HWND main = main_window();
    if (!main || !IsWindowVisible(main) || !IsWindowEnabled(main) || FindWindowW(0, EULA_TITLE) ||
        FindWindowW(0, DESTINATION_TITLE) || FindWindowW(0, ERROR_TITLE)) {
        menu_ticks = 0;
        return;
    }
    menu_ticks++;
    if (license_answered) {
        if (menu_ticks >= MENU_AFTER_LICENSE_TICKS) ExitProcess(MENU_EXIT);
        return;
    }
    if (menu_clicks < MENU_CLICKS) {
        if (menu_ticks < MENU_SETTLE_TICKS) return;
        LPARAM position = (180L << 16) | 200L;
        PostMessageW(main, 0x0200, 0, position);
        PostMessageW(main, 0x0201, 1, position);
        PostMessageW(main, 0x0202, 0, position);
        menu_clicks++;
        menu_ticks = 0;
    } else if (menu_ticks >= MENU_LAST_CLICK_TICKS) {
        ExitProcess(MENU_EXIT);
    }
}

void __stdcall entry(void) {
    HWND eula = 0;
    while (!eula) {
        exit_on_error_dialog();
        eula = FindWindowW(0, EULA_TITLE);
        if (eula) break;
        exit_when_installer_finished(BEFORE_LICENSE_ABSENT_TICKS);
        answer_menu(0);
        Sleep(100);
    }
    HWND agree = find_button(eula, AGREE_TEXT, 8);
    if (!agree) ExitProcess(12);
    PostMessageW(agree, 0x00f5, 0, 0);
    menu_ticks = 0;
    for (;;) {
        HWND destination = FindWindowW(0, DESTINATION_TITLE);
        if (destination) {
            HWND ok = find_button(destination, OK_TEXT, 8);
            if (!ok) ExitProcess(13);
            PostMessageW(ok, 0x00f5, 0, 0);
            ExitProcess(0);
        }
        exit_on_error_dialog();
        exit_when_installer_finished(AFTER_LICENSE_ABSENT_TICKS);
        answer_menu(1);
        Sleep(100);
    }
}
