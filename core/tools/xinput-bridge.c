typedef unsigned char BYTE;
typedef unsigned short WORD;
typedef unsigned short WCHAR;
typedef short SHORT;
typedef unsigned int UINT;
typedef unsigned long DWORD;
typedef long LONG;
typedef int BOOL;
typedef void *HANDLE;
typedef void *HINSTANCE;
typedef WCHAR *LPWSTR;
typedef void *LPVOID;

__declspec(dllimport) DWORD __stdcall GetTickCount(void);
__declspec(dllimport) DWORD __stdcall GetEnvironmentVariableA(const char *, char *, DWORD);
__declspec(dllimport) HANDLE __stdcall CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD,
                                                   HANDLE);
__declspec(dllimport) HANDLE __stdcall CreateFileMappingA(HANDLE, void *, DWORD, DWORD, DWORD,
                                                          const char *);
__declspec(dllimport) void *__stdcall MapViewOfFile(HANDLE, DWORD, DWORD, DWORD, DWORD);
__declspec(dllimport) BOOL __stdcall CloseHandle(HANDLE);
__declspec(dllimport) BOOL __stdcall DisableThreadLibraryCalls(HINSTANCE);

#define TRUE 1
#define FALSE 0
#define ERROR_SUCCESS 0UL
#define ERROR_BAD_ARGUMENTS 160UL
#define ERROR_DEVICE_NOT_CONNECTED 1167UL
#define ERROR_EMPTY 4306UL
#define GENERIC_READ 0x80000000UL
#define FILE_SHARE_READ 0x1UL
#define FILE_SHARE_WRITE 0x2UL
#define FILE_SHARE_DELETE 0x4UL
#define OPEN_EXISTING 3UL
#define PAGE_READONLY 0x2UL
#define FILE_MAP_READ 0x4UL
#define INVALID_HANDLE_VALUE ((HANDLE)(long)-1)
#define DLL_PROCESS_ATTACH 1UL

/* The compiler's full barrier and atomic exchange (no C runtime: Windows' MemoryBarrier and
   InterlockedExchange are compiler intrinsics too). */
#define MemoryBarrier() __atomic_thread_fence(__ATOMIC_SEQ_CST)
#define InterlockedExchange(target, value) __atomic_exchange_n((target), (value), __ATOMIC_SEQ_CST)

/*
 * xinput1_4.dll for the Android app: controller 0 is the Android app's gamepad.
 *
 * The app writes the pad's state into a small file (PD2_GAMEPAD_FILE, a Unix path) and this
 * DLL maps the same file, so every XInputGetState call reads the current state without any
 * device access: an Android app cannot give Wine an evdev, hidraw or udev device.
 *
 * File layout (little endian, 32 bytes), written by the app:
 *   0  u32 magic 'PD2G'      4  u32 packet (seqlock: odd while the fields change)
 *   8  u32 connected (0/1)   12 u16 XInput buttons
 *   14 u8  left trigger      15 u8  right trigger
 *   16 i16 lx, ly, rx, ry (XInput signs: up is positive)
 *   24..31 reserved
 *
 * A freestanding i686 DLL with no C runtime, built by tools/build-sealed-helper.sh with the
 * exports in tools/xinput-bridge.def, with its own declarations in place of <windows.h>.
 */

#define XUSER_MAX_COUNT 4
#define BRIDGE_MAGIC 0x47324450u /* "PD2G" */

typedef struct {
    WORD wButtons;
    BYTE bLeftTrigger, bRightTrigger;
    SHORT sThumbLX, sThumbLY, sThumbRX, sThumbRY;
} XINPUT_GAMEPAD;

typedef struct { DWORD dwPacketNumber; XINPUT_GAMEPAD Gamepad; } XINPUT_STATE;
typedef struct { WORD wLeftMotorSpeed, wRightMotorSpeed; } XINPUT_VIBRATION;
typedef struct {
    BYTE Type, SubType;
    WORD Flags;
    XINPUT_GAMEPAD Gamepad;
    XINPUT_VIBRATION Vibration;
} XINPUT_CAPABILITIES;
typedef struct { BYTE BatteryType, BatteryLevel; } XINPUT_BATTERY_INFORMATION;
typedef struct { WORD VirtualKey; WCHAR Unicode; WORD Flags; BYTE UserIndex, HidCode; } XINPUT_KEYSTROKE;

typedef struct {
    volatile DWORD magic, packet, connected;
    volatile WORD buttons;
    volatile BYTE lt, rt;
    volatile SHORT lx, ly, rx, ry;
    DWORD reserved[2];
} BridgeState;

_Static_assert(sizeof(BridgeState) == 32, "the gamepad file is 32 bytes");

static const BridgeState *volatile bridge;
static BOOL enabled = TRUE;
static LONG opening;
static DWORD last_try;

/* Windows' ZeroMemory, without the C runtime's memset. */
static void ZeroMemory(void *target, UINT size)
{
    volatile BYTE *bytes = target;
    while (size--) *bytes++ = 0;
}

/* Maps the app's file. A missing file only means: no controller yet; try again at most once
   a second, so a game started before the app wrote the file still finds the pad. */
static const BridgeState *open_bridge(void)
{
    char unix_path[1024], path[1100];
    HANDLE file, mapping;
    DWORD n, i, now = GetTickCount();
    if (bridge) return bridge;
    if (last_try && now - last_try < 1000) return 0;
    if (InterlockedExchange(&opening, 1)) return 0;
    last_try = now ? now : 1;
    n = GetEnvironmentVariableA("PD2_GAMEPAD_FILE", unix_path, sizeof unix_path);
    if (n == 0 || n >= sizeof unix_path || unix_path[0] != '/') { opening = 0; return 0; }
    path[0] = 'Z'; path[1] = ':';
    for (i = 0; i <= n; i++) path[i + 2] = unix_path[i] == '/' ? '\\' : unix_path[i];
    file = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                       0, OPEN_EXISTING, 0, 0);
    if (file == INVALID_HANDLE_VALUE) { opening = 0; return 0; }
    mapping = CreateFileMappingA(file, 0, PAGE_READONLY, 0, sizeof(BridgeState), 0);
    CloseHandle(file);
    if (mapping) {
        bridge = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, sizeof(BridgeState));
        CloseHandle(mapping);
    }
    opening = 0;
    return bridge;
}

static BOOL pad_connected(DWORD user)
{
    const BridgeState *s;
    if (user != 0) return FALSE;
    s = open_bridge();
    return s && s->magic == BRIDGE_MAGIC && s->connected;
}

DWORD __stdcall XInputGetState(DWORD user, XINPUT_STATE *state)
{
    const BridgeState *s;
    DWORD packet, tries = 0;
    if (user >= XUSER_MAX_COUNT || !state) return ERROR_BAD_ARGUMENTS;
    if (!pad_connected(user)) return ERROR_DEVICE_NOT_CONNECTED;
    s = bridge;
    /* A seqlock: the packet number is odd while the app writes the fields; read again if it
       was odd or changed (a bounded number of times: a stopped app must not hang the game). */
    do {
        packet = s->packet;
        MemoryBarrier();
        state->dwPacketNumber = packet;
        state->Gamepad.wButtons = s->buttons;
        state->Gamepad.bLeftTrigger = s->lt;
        state->Gamepad.bRightTrigger = s->rt;
        state->Gamepad.sThumbLX = s->lx;
        state->Gamepad.sThumbLY = s->ly;
        state->Gamepad.sThumbRX = s->rx;
        state->Gamepad.sThumbRY = s->ry;
        MemoryBarrier();
    } while (((packet & 1) || packet != s->packet) && ++tries < 1000);
    /* After 1000 tries (the app stopped mid-write) the state may be torn; that is still a
       valid pad state, and better than hanging the game. */
    if (!enabled) ZeroMemory(&state->Gamepad, sizeof state->Gamepad);
    return ERROR_SUCCESS;
}

DWORD __stdcall XInputGetStateEx(DWORD user, XINPUT_STATE *state)
{
    return XInputGetState(user, state);
}

DWORD __stdcall XInputSetState(DWORD user, XINPUT_VIBRATION *vibration)
{
    if (user >= XUSER_MAX_COUNT || !vibration) return ERROR_BAD_ARGUMENTS;
    return pad_connected(user) ? ERROR_SUCCESS : ERROR_DEVICE_NOT_CONNECTED;
}

DWORD __stdcall XInputGetCapabilities(DWORD user, DWORD flags, XINPUT_CAPABILITIES *caps)
{
    (void)flags;
    if (user >= XUSER_MAX_COUNT || !caps) return ERROR_BAD_ARGUMENTS;
    if (!pad_connected(user)) return ERROR_DEVICE_NOT_CONNECTED;
    ZeroMemory(caps, sizeof *caps);
    caps->Type = 1;    /* XINPUT_DEVTYPE_GAMEPAD */
    caps->SubType = 1; /* XINPUT_DEVSUBTYPE_GAMEPAD */
    caps->Gamepad.wButtons = 0xf3ff;
    caps->Gamepad.bLeftTrigger = caps->Gamepad.bRightTrigger = 0xff;
    caps->Gamepad.sThumbLX = caps->Gamepad.sThumbLY = (SHORT)0xffc0;
    caps->Gamepad.sThumbRX = caps->Gamepad.sThumbRY = (SHORT)0xffc0;
    return ERROR_SUCCESS;
}

void __stdcall XInputEnable(BOOL enable) { enabled = enable; }

DWORD __stdcall XInputGetBatteryInformation(DWORD user, BYTE type, XINPUT_BATTERY_INFORMATION *info)
{
    (void)type;
    if (user >= XUSER_MAX_COUNT || !info) return ERROR_BAD_ARGUMENTS;
    if (!pad_connected(user)) return ERROR_DEVICE_NOT_CONNECTED;
    info->BatteryType = 1;  /* BATTERY_TYPE_WIRED */
    info->BatteryLevel = 3; /* BATTERY_LEVEL_FULL */
    return ERROR_SUCCESS;
}

DWORD __stdcall XInputGetKeystroke(DWORD user, DWORD reserved, XINPUT_KEYSTROKE *keystroke)
{
    (void)reserved;
    if (!keystroke) return ERROR_BAD_ARGUMENTS;
    return pad_connected(user == 0xff ? 0 : user) ? ERROR_EMPTY : ERROR_DEVICE_NOT_CONNECTED;
}

DWORD __stdcall XInputGetAudioDeviceIds(DWORD user, LPWSTR render, UINT *render_count, LPWSTR capture, UINT *capture_count)
{
    (void)render;
    (void)capture;
    if (user >= XUSER_MAX_COUNT) return ERROR_BAD_ARGUMENTS;
    if (!pad_connected(user)) return ERROR_DEVICE_NOT_CONNECTED;
    if (render_count) *render_count = 0;
    if (capture_count) *capture_count = 0;
    return ERROR_SUCCESS;
}

BOOL __stdcall DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved)
{
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) DisableThreadLibraryCalls(instance);
    return TRUE;
}
