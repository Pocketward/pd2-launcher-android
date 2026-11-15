typedef void *HWND;
typedef unsigned short WCHAR;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef long LPARAM;
typedef unsigned long WPARAM;
typedef void *HANDLE;
typedef int BOOL;

__declspec(dllimport) HWND __stdcall FindWindowW(const WCHAR *, const WCHAR *);
__declspec(dllimport) HWND __stdcall FindWindowExW(HWND, HWND, const WCHAR *, const WCHAR *);
__declspec(dllimport) int __stdcall GetWindowTextW(HWND, WCHAR *, int);
__declspec(dllimport) DWORD __stdcall GetWindowThreadProcessId(HWND, DWORD *);
__declspec(dllimport) HANDLE __stdcall OpenProcess(DWORD, BOOL, DWORD);
__declspec(dllimport) BOOL __stdcall TerminateProcess(HANDLE, UINT);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) void __stdcall Sleep(DWORD);
__declspec(dllimport) void __stdcall ExitProcess(UINT);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) int __stdcall WideCharToMultiByte(UINT, DWORD, const WCHAR *, int, char *, int,
                                                        const char *, BOOL *);

#define SYNCHRONIZE_AND_TERMINATE 0x00100001UL
#define WAIT_OBJECT_0 0
#define STD_OUTPUT_HANDLE ((DWORD)-11)
#define CP_UTF8 65001

/*
 * Blizzard Downloader 2.2 (the D2 and LoD wrappers) run with
 * `--saveas=<folder> --autolaunch=0`: the folder skips its "Browse for
 * Folder" prompt, and the download does not start the installer by itself.
 * Its only window is a dialog (class #32770) titled "<N>% - Blizzard
 * Downloader", which stays open at "100% - Blizzard Downloader" when the
 * download is complete; closing it would still start the unverified
 * installer. Once that exact window has shown 100% for two seconds, this
 * helper terminates the process that owns it, so nothing is launched; the
 * Launcher, not this helper, then verifies every downloaded file against its
 * frozen identity before the installer runs.
 *
 * Exit codes: 0 the complete download was stopped, or the downloader ended
 * on its own (the verification decides); 21 no downloader window within ten
 * minutes; 22 a folder prompt appeared (the folder argument was not
 * honoured); 23 the downloader showed a message box (an error: its progress
 * dialog has no OK button); 24-26 the downloader could not be stopped.
 *
 * Before it exits 23, the helper writes the message box's text to standard
 * output as one line of at most 1024 bytes of UTF-8:
 *
 *   downloader message: <title>: <text>
 *
 * <text> is the text of every Static control of the box that has text,
 * joined by spaces, each line break or other control character made one
 * space. The helper writes nothing in the other cases.
 */
static const WCHAR DIALOG_CLASS[] = L"#32770";
static const WCHAR TITLE_SUFFIX[] = L" - Blizzard Downloader";
static const WCHAR PLAIN_TITLE[] = L"Blizzard Downloader";
static const WCHAR COMPLETE_TITLE[] = L"100% - Blizzard Downloader";
static const WCHAR FOLDER_TITLE[] = L"Browse for Folder";
static const WCHAR FOLDER_TITLE_UPPER[] = L"Browse For Folder";
static const WCHAR OK_TEXT[] = L"OK";
static const WCHAR STATIC_CLASS[] = L"Static";
static const WCHAR MESSAGE_PREFIX[] = L"downloader message: ";

#define MESSAGE_BYTES 1024
/* One UTF-16 unit is at most three bytes of UTF-8. */
static WCHAR message[MESSAGE_BYTES];
static int message_length;
static char message_utf8[3 * MESSAGE_BYTES];

#define NEVER_SEEN_TICKS 6000
#define ABSENT_TICKS 50
#define COMPLETE_TICKS 20

static int equal_text(const WCHAR *left, const WCHAR *right) {
    while (*left && *left == *right) {
        left++;
        right++;
    }
    return *left == *right;
}

/* "<digits>% - Blizzard Downloader", or exactly "Blizzard Downloader". */
static int downloader_title(const WCHAR *title) {
    if (equal_text(title, PLAIN_TITLE)) return 1;
    const WCHAR *cursor = title;
    int digits = 0;
    while (*cursor >= L'0' && *cursor <= L'9' && digits < 3) {
        cursor++;
        digits++;
    }
    if (!digits || *cursor != L'%') return 0;
    return equal_text(cursor + 1, TITLE_SUFFIX);
}

static int has_ok_button(HWND dialog) {
    HWND previous = 0;
    for (int count = 0; count < 64; count++) {
        HWND child = FindWindowExW(dialog, previous, 0, 0);
        if (!child) return 0;
        previous = child;
        WCHAR text[16] = {0};
        GetWindowTextW(child, text, 16);
        const WCHAR *label = text;
        if (*label == L'&') label++;
        if (equal_text(label, OK_TEXT)) return 1;
    }
    return 0;
}

static void append(const WCHAR *text) {
    for (; *text && message_length < MESSAGE_BYTES - 1; text++) {
        if (*text == L'\r') continue;
        message[message_length++] = *text < L' ' ? L' ' : *text;
    }
}

/* The box's title and Static texts, cut at a character to fit MESSAGE_BYTES. */
static void write_message(HWND dialog, const WCHAR *title) {
    append(MESSAGE_PREFIX);
    append(title);
    const WCHAR *separator = L": ";
    HWND previous = 0;
    for (int count = 0; count < 64; count++) {
        HWND child = FindWindowExW(dialog, previous, STATIC_CLASS, 0);
        if (!child) break;
        previous = child;
        WCHAR text[512];
        if (GetWindowTextW(child, text, 512) <= 0) continue;
        append(separator);
        append(text);
        separator = L" ";
    }
    int length = WideCharToMultiByte(CP_UTF8, 0, message, message_length, message_utf8,
                                     sizeof(message_utf8), 0, 0);
    if (length > MESSAGE_BYTES - 1) {
        length = MESSAGE_BYTES - 1;
        while (length > 0 && ((unsigned char)message_utf8[length] & 0xC0) == 0x80) length--;
    }
    message_utf8[length++] = '\n';
    DWORD written = 0;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), message_utf8, (DWORD)length, &written, 0);
}

void __stdcall entry(void) {
    int seen = 0;
    int unseen_ticks = 0;
    int absent_ticks = 0;
    int complete_ticks = 0;
    for (;;) {
        if (FindWindowW(DIALOG_CLASS, FOLDER_TITLE) || FindWindowW(DIALOG_CLASS, FOLDER_TITLE_UPPER))
            ExitProcess(22);
        int present = 0;
        HWND previous = 0;
        for (int count = 0; count < 256; count++) {
            HWND dialog = FindWindowExW(0, previous, DIALOG_CLASS, 0);
            if (!dialog) break;
            previous = dialog;
            WCHAR title[64];
            if (GetWindowTextW(dialog, title, 64) <= 0 || !downloader_title(title)) continue;
            present = 1;
            if (has_ok_button(dialog)) {
                write_message(dialog, title);
                ExitProcess(23);
            }
            if (!equal_text(title, COMPLETE_TITLE)) {
                complete_ticks = 0;
            } else if (++complete_ticks >= COMPLETE_TICKS) {
                DWORD owner = 0;
                GetWindowThreadProcessId(dialog, &owner);
                HANDLE process = owner ? OpenProcess(SYNCHRONIZE_AND_TERMINATE, 0, owner) : 0;
                if (!process) ExitProcess(24);
                if (!TerminateProcess(process, 0)) ExitProcess(25);
                if (WaitForSingleObject(process, 10000) != WAIT_OBJECT_0) ExitProcess(26);
                ExitProcess(0);
            }
        }
        if (present) {
            seen = 1;
            absent_ticks = 0;
        } else if (seen) {
            if (++absent_ticks >= ABSENT_TICKS) ExitProcess(0);
        } else if (++unseen_ticks >= NEVER_SEEN_TICKS) {
            ExitProcess(21);
        }
        Sleep(100);
    }
}
