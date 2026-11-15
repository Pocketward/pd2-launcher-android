typedef void *HWND;
typedef void *HDC;
typedef void *HGLRC;
typedef void *HANDLE;
typedef void *HINSTANCE;
typedef void *HMENU;
typedef unsigned short WCHAR;
typedef unsigned short WORD;
typedef unsigned char BYTE;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef int BOOL;

typedef struct {
    WORD nSize;
    WORD nVersion;
    DWORD dwFlags;
    BYTE iPixelType;
    BYTE cColorBits;
    BYTE cRedBits, cRedShift, cGreenBits, cGreenShift, cBlueBits, cBlueShift;
    BYTE cAlphaBits, cAlphaShift;
    BYTE cAccumBits, cAccumRedBits, cAccumGreenBits, cAccumBlueBits, cAccumAlphaBits;
    BYTE cDepthBits, cStencilBits, cAuxBuffers;
    BYTE iLayerType;
    BYTE bReserved;
    DWORD dwLayerMask, dwVisibleMask, dwDamageMask;
} PIXELFORMATDESCRIPTOR;

__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(UINT);
__declspec(dllimport) HWND __stdcall CreateWindowExW(DWORD, const WCHAR *, const WCHAR *, DWORD,
                                                     int, int, int, int, HWND, HMENU,
                                                     HINSTANCE, void *);
__declspec(dllimport) BOOL __stdcall DestroyWindow(HWND);
__declspec(dllimport) HDC __stdcall GetDC(HWND);
__declspec(dllimport) int __stdcall ReleaseDC(HWND, HDC);
__declspec(dllimport) int __stdcall ChoosePixelFormat(HDC, const PIXELFORMATDESCRIPTOR *);
__declspec(dllimport) BOOL __stdcall SetPixelFormat(HDC, int, const PIXELFORMATDESCRIPTOR *);
__declspec(dllimport) HGLRC __stdcall wglCreateContext(HDC);
__declspec(dllimport) BOOL __stdcall wglMakeCurrent(HDC, HGLRC);
__declspec(dllimport) BOOL __stdcall wglDeleteContext(HGLRC);
__declspec(dllimport) const BYTE *__stdcall glGetString(UINT);

/*
 * Report the OpenGL renderer the game would draw with. Diablo II draws
 * through D2GL (its Glide renderer), which is a 32-bit OpenGL client, so
 * this helper is a 32-bit program asking the same Wine opengl32 in the same
 * prefix and session: it creates a never-shown window, gives it a
 * double-buffered RGBA pixel format, makes a legacy context current, and
 * writes three lines to standard output:
 *
 *   renderer=<GL_RENDERER>
 *   vendor=<GL_VENDOR>
 *   version=<GL_VERSION>
 *
 * The Launcher classifies the renderer (Mesa's llvmpipe, softpipe, or
 * swrast is software rendering); this helper judges nothing.
 *
 * Exit codes: 0 the three lines were written; 31 no window; 32 no pixel
 * format; 33 no OpenGL context; 34 the context could not be made current;
 * 35 no renderer string; 36 the lines could not be written.
 */
#define STD_OUTPUT_HANDLE ((DWORD)-11)
#define WS_POPUP 0x80000000UL
#define PFD_DRAW_TO_WINDOW 0x04
#define PFD_SUPPORT_OPENGL 0x20
#define PFD_DOUBLEBUFFER 0x01
#define GL_VENDOR 0x1F00
#define GL_RENDERER 0x1F01
#define GL_VERSION 0x1F02

static const WCHAR STATIC_CLASS[] = L"STATIC";
static const PIXELFORMATDESCRIPTOR FORMAT = {
    sizeof(PIXELFORMATDESCRIPTOR), 1,
    PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
    0 /* PFD_TYPE_RGBA */, 32,
    0, 0, 0, 0, 0, 0, 8, 0,
    0, 0, 0, 0, 0,
    24, 8, 0,
    0 /* PFD_MAIN_PLANE */, 0,
    0, 0, 0,
};

static char output[1024];
static DWORD length;

static void append(const char *text) {
    while (*text && length < sizeof(output) - 1) {
        char c = *text++;
        /* One line per value: a renderer string never ends a line early. */
        output[length++] = (c == '\n' || c == '\r') ? ' ' : c;
    }
}

static void line(const char *name, const BYTE *value) {
    append(name);
    append(value ? (const char *)value : "");
    if (length < sizeof(output) - 1) output[length++] = '\n';
}

void __stdcall entry(void) {
    HWND window = CreateWindowExW(0, STATIC_CLASS, STATIC_CLASS, WS_POPUP, 0, 0, 16, 16, 0, 0,
                                  0, 0);
    if (!window) ExitProcess(31);
    HDC device = GetDC(window);
    int format = device ? ChoosePixelFormat(device, &FORMAT) : 0;
    if (!format || !SetPixelFormat(device, format, &FORMAT)) ExitProcess(32);
    HGLRC context = wglCreateContext(device);
    if (!context) ExitProcess(33);
    if (!wglMakeCurrent(device, context)) ExitProcess(34);
    const BYTE *renderer = glGetString(GL_RENDERER);
    if (!renderer || !*renderer) ExitProcess(35);
    line("renderer=", renderer);
    line("vendor=", glGetString(GL_VENDOR));
    line("version=", glGetString(GL_VERSION));
    wglMakeCurrent(0, 0);
    wglDeleteContext(context);
    ReleaseDC(window, device);
    DestroyWindow(window);
    DWORD written = 0;
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    if (!WriteFile(out, output, length, &written, 0) || written != length) ExitProcess(36);
    ExitProcess(0);
}
