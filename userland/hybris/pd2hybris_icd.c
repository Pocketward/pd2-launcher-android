/*
 * libvulkan_pd2hybris.so: a Vulkan ICD for the glibc userland that forwards to the phone's own
 * Vulkan HAL (/vendor/lib64/hw/vulkan.<name>.so), loaded through libhybris's bionic linker.
 *
 * Research prototype (research/android-libhybris-pvr). Written from the Vulkan specification,
 * the Khronos loader-ICD interface (LoaderDriverInterface.md, vk_icd.h) and AOSP's HAL contract
 * (hardware/libhardware: hardware.h, hwvulkan.h).
 *
 * What the Android Vulkan loader normally adds on top of the HAL, this ICD adds for X11:
 *   - VK_KHR_surface, VK_KHR_xlib_surface, VK_KHR_xcb_surface (loader-created surfaces), and
 *   - VK_KHR_swapchain as a software swapchain: images are the driver's own images. A present
 *     blits the image into one of the app's AHardwareBuffers (the direct present, the app's
 *     present server, PD2_PRESENT_SOCKET; Mesa patch 0004's protocol), or copies it into a
 *     host-cached buffer and puts it on the X window (xcb_put_image) while the app wants the X
 *     screen, as Mesa's software WSI does; the waits are on the swapchain's present thread.
 * The HAL's own WSI-related extensions are hidden (VK_ANDROID_native_buffer and such).
 * A fault in the phone's driver, libhybris or this ICD writes a "fault" status line before Wine
 * gets the signal (the fault watch, fault_report.h).
 *
 * Backends:
 *   PD2_HYBRIS_BACKEND=hybris (default on arm64): the HAL through libhybris.
 *   PD2_HYBRIS_BACKEND=glibc: a glibc ICD (PD2_HYBRIS_DESK_ICD, e.g. lavapipe) for the desk test
 *   of the WSI part.
 * Other environment: PD2_HYBRIS_HAL (path of the HAL), PD2_HYBRIS_DEBUG=1 (log to stderr),
 * PD2_HYBRIS_NAMESPACE=sphal|default (namespace for the HAL), PD2_HYBRIS_STATUS_FILE (the
 * route's status lines for the app), PD2_PRESENT_COUNTER (the app's present counter page: each
 * frame counted, every present on the app's clock, pacing by the Game view, direct present wanted;
 * present_counter.h), PD2_PRESENT_SOCKET (the app's present server), PD2_HYBRIS_NO_DIRECT=1 (the
 * X screen only, tests), PD2_HYBRIS_TEST_FAIL / PD2_HYBRIS_TEST_FAULT (the app's fallback, tests).
 * In the app's userland (userland/build-hybris.sh) this ICD is the libhybris route
 * (GraphicsRoute.LIBHYBRIS): libhybris-common.so.1 finds its linker q.so in HYBRIS_LINKER_DIR.
 */
#define _GNU_SOURCE
#define VK_NO_PROTOTYPES
#define VK_USE_PLATFORM_XLIB_KHR 1
#define VK_USE_PLATFORM_XCB_KHR 1
#define VK_USE_PLATFORM_ANDROID_KHR 1 /* the AHardwareBuffer import of the direct present */
#include <X11/Xlib.h>
#include <X11/Xlib-xcb.h>
#include <xcb/xcb.h>
#include <vulkan/vulkan.h>
#include <vulkan/vk_icd.h>

#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <inttypes.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>

#define PD2_EXPORT __attribute__((visibility("default")))
#define ARRAY_LEN(a) (sizeof(a) / sizeof((a)[0]))

/* ------------------------------------------------------------------ logging */

static int g_debug = -1;
static void logf_(int always, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void logf_(int always, const char *fmt, ...)
{
    if (g_debug < 0) {
        const char *e = getenv("PD2_HYBRIS_DEBUG");
        g_debug = e && *e && strcmp(e, "0") != 0;
    }
    if (!always && !g_debug)
        return;
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "pd2hybris: ");
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
}
#define LOGD(...) logf_(0, __VA_ARGS__)
#define LOGE(...) logf_(1, __VA_ARGS__)

/* ------------------------------------------------------------------ fault report (debug) */

#include "fault_report.h"
#include "present_counter.h"

/* ------------------------------------------------------------------ the route's status file */

/* PD2_HYBRIS_STATUS_FILE: one line per event that the app reads (SessionService: a failure of the
 * phone's driver on the libhybris route), "<process> <pid> ok <what>" or "<process> <pid> failed
 * <why>", appended. Every Wine process that loads the ICD writes; the app takes Game.exe's. */
static char g_be_why[256];

static void status_line(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void status_line(const char *fmt, ...)
{
    const char *path = getenv("PD2_HYBRIS_STATUS_FILE");
    if (!path || !*path)
        return;
    char comm[32] = "?", text[512];
    int cf = open("/proc/self/comm", O_RDONLY | O_CLOEXEC);
    if (cf >= 0) {
        ssize_t n = read(cf, comm, sizeof comm - 1);
        comm[n > 0 ? n : 0] = 0;
        char *nl = strchr(comm, '\n');
        if (nl)
            *nl = 0;
        close(cf);
    }
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(text, sizeof text, fmt, ap);
    va_end(ap);
    for (char *c = text; *c; c++)
        if (*c == '\n')
            *c = ' ';
    char line[600];
    int len = snprintf(line, sizeof line, "%s %ld %s\n", comm[0] ? comm : "?", (long)getpid(), text);
    if (len >= (int)sizeof line)
        len = (int)sizeof line - 1;
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
    if (fd < 0)
        return;
    /* One write: lines of several processes do not mix. A failed write changes nothing for the game. */
    if (len > 0 && write(fd, line, (size_t)len) < 0)
        LOGD("status file not written: %s", strerror(errno));
    close(fd);
}

/* Logs [fmt] and keeps it as the reason the backend did not start (status_line). */
static void fail_why(const char *fmt, ...) __attribute__((format(printf, 1, 2), unused));
static void fail_why(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(g_be_why, sizeof g_be_why, fmt, ap);
    va_end(ap);
    LOGE("%s", g_be_why);
}

/* ------------------------------------------------------------------ the HAL contract (AOSP) */

/* hardware/libhardware/include/hardware/hardware.h, LP64 layout */
struct hw_module_t;
struct hw_device_t;
struct hw_module_methods_t {
    int (*open)(const struct hw_module_t *module, const char *id, struct hw_device_t **device);
};
struct hw_module_t {
    uint32_t tag;
    uint16_t module_api_version;
    uint16_t hal_api_version;
    const char *id;
    const char *name;
    const char *author;
    struct hw_module_methods_t *methods;
    void *dso;
    uint64_t reserved[32 - 7];
};
struct hw_device_t {
    uint32_t tag;
    uint32_t version;
    struct hw_module_t *module;
    uint64_t reserved[12];
    int (*close)(struct hw_device_t *device);
};
#define HARDWARE_MODULE_TAG 0x48574d54u /* MAKE_TAG_CONSTANT("H","W","M","T") */
#define HARDWARE_DEVICE_TAG 0x48574454u /* MAKE_TAG_CONSTANT("H","W","D","T") */
/* hardware/libhardware/include/hardware/hwvulkan.h */
typedef struct {
    struct hw_device_t common;
    PFN_vkEnumerateInstanceExtensionProperties EnumerateInstanceExtensionProperties;
    PFN_vkCreateInstance CreateInstance;
    PFN_vkGetInstanceProcAddr GetInstanceProcAddr;
} hwvulkan_device_t;
#define HWVULKAN_DISPATCH_MAGIC 0xbaadf00dUL

/* bionic's android/dlext.h */
typedef struct {
    uint64_t flags;
    void *reserved_addr;
    size_t reserved_size;
    int relro_fd;
    int library_fd;
    int64_t library_fd_offset;
    void *library_namespace;
} android_dlextinfo;
#define ANDROID_DLEXT_USE_NAMESPACE 0x200

/* ------------------------------------------------------------------ backend */

struct backend {
    int kind; /* 1 hybris, 2 glibc */
    PFN_vkGetInstanceProcAddr gipa;
    PFN_vkEnumerateInstanceExtensionProperties enum_inst_ext;
    PFN_vkCreateInstance create_instance;
    PFN_vkEnumerateInstanceVersion enum_inst_version;
    char hal_path[512];
};
static struct backend g_be;
static pthread_once_t g_be_once = PTHREAD_ONCE_INIT;
static int g_be_ok;

#if defined(__aarch64__) && !defined(PD2_NO_HYBRIS)
/* libhybris-common's exports (hybris/common/hooks.c) */
extern void *hybris_dlopen(const char *filename, int flag);
extern void *hybris_dlsym(void *handle, const char *symbol);
extern char *hybris_dlerror(void);
extern void *android_dlopen_ext(const char *filename, int flag, const void *extinfo);
extern void *android_get_exported_namespace(const char *name);
#define HAVE_HYBRIS 1
#endif

#ifdef HAVE_HYBRIS
extern void hybris_set_hook_callback(void *(*cb)(const char *symbol, const char *requester));

/* Consulted by libhybris's linker before its own hook table, for every symbol of every library it
 * loads. libhybris leaves the signal calls to bionic's libc (its signal hooks are off: the types
 * differ), so vendor code would install handlers and masks behind glibc's back; these three are
 * translated here instead. PD2_HYBRIS_TRACE_SYMS=<file> lists every lookup. */
static FILE *g_symtrace;
static void *hook_cb(const char *sym, const char *requester)
{
    if (g_symtrace)
        fprintf(g_symtrace, "%s %s\n", sym, requester ? requester : "?");
    if (strcmp(sym, "sigaction") == 0 || strcmp(sym, "sigaction64") == 0)
        return (void *)pd2_bionic_sigaction;
    if (strcmp(sym, "pthread_sigmask") == 0 || strcmp(sym, "pthread_sigmask64") == 0)
        return (void *)pd2_bionic_pthread_sigmask;
    if (strcmp(sym, "sigprocmask") == 0 || strcmp(sym, "sigprocmask64") == 0)
        return (void *)pd2_bionic_sigprocmask;
    return NULL;
}

static int find_hal(char *out, size_t n)
{
    const char *e = getenv("PD2_HYBRIS_HAL");
    if (e && *e) {
        snprintf(out, n, "%s", e);
        return 0;
    }
    /* hw_get_module's variants come from properties (ro.hardware.vulkan first); the folder
     * has one vulkan.*.so on the phones we know, so the first match is taken. */
    static const char *dirs[] = { "/vendor/lib64/hw", "/odm/lib64/hw" };
    for (size_t i = 0; i < ARRAY_LEN(dirs); i++) {
        DIR *d = opendir(dirs[i]);
        if (!d)
            continue;
        struct dirent *de;
        while ((de = readdir(d))) {
            size_t l = strlen(de->d_name);
            if (strncmp(de->d_name, "vulkan.", 7) == 0 && l > 10 && strcmp(de->d_name + l - 3, ".so") == 0) {
                snprintf(out, n, "%s/%s", dirs[i], de->d_name);
                closedir(d);
                return 0;
            }
        }
        closedir(d);
    }
    return -1;
}

static hwvulkan_device_t *g_hal;

static void *hal_open_library(const char *path)
{
    const char *ns_name = getenv("PD2_HYBRIS_NAMESPACE");
    if (!ns_name || !*ns_name)
        ns_name = "sphal";
    void *h = NULL;
    if (strcmp(ns_name, "default") != 0) {
        void *ns = android_get_exported_namespace(ns_name);
        LOGD("exported namespace %s: %p", ns_name, ns);
        if (ns) {
            android_dlextinfo ext;
            memset(&ext, 0, sizeof ext);
            ext.flags = ANDROID_DLEXT_USE_NAMESPACE;
            ext.library_namespace = ns;
            h = android_dlopen_ext(path, RTLD_NOW | RTLD_LOCAL, &ext);
            if (!h)
                LOGE("android_dlopen_ext(%s, %s): %s", path, ns_name, hybris_dlerror());
        }
    }
    if (!h) {
        h = hybris_dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h)
            fail_why("the phone's Vulkan driver %s did not load: %s", path, hybris_dlerror());
    }
    return h;
}

/* A glibc process never sets bionic's `environ`; some vendor code reads it directly. */
static void bionic_environ_fixup(void)
{
    extern char **environ;
    void *libc = hybris_dlopen("libc.so", RTLD_NOW);
    if (!libc)
        return;
    char ***benv = (char ***)hybris_dlsym(libc, "environ");
    if (benv && !*benv) {
        *benv = environ;
        LOGD("bionic environ set");
    }
}

static int backend_init_hybris(void)
{
    if (find_hal(g_be.hal_path, sizeof g_be.hal_path) != 0) {
        fail_why("no Vulkan HAL found (/vendor/lib64/hw/vulkan.*.so)");
        return -1;
    }
    const char *tr = getenv("PD2_HYBRIS_TRACE_SYMS");
    if (tr && *tr) {
        g_symtrace = fopen(tr, "w");
        if (g_symtrace)
            setvbuf(g_symtrace, NULL, _IOLBF, 0);
    }
    hybris_set_hook_callback(hook_cb);
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    void *h = hal_open_library(g_be.hal_path);
    if (!h)
        return -1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    LOGD("loaded %s in %ld ms", g_be.hal_path,
         (long)((t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000));
    struct hw_module_t *m = (struct hw_module_t *)hybris_dlsym(h, "HMI");
    if (!m) {
        fail_why("%s has no HMI: %s", g_be.hal_path, hybris_dlerror());
        return -1;
    }
    LOGD("HMI tag 0x%x id %s name %s author %s api %u.%u", m->tag, m->id ? m->id : "?",
         m->name ? m->name : "?", m->author ? m->author : "?", m->module_api_version >> 8,
         m->module_api_version & 0xff);
    if (m->tag != HARDWARE_MODULE_TAG || !m->methods || !m->methods->open) {
        fail_why("HMI of %s is not a hw module", g_be.hal_path);
        return -1;
    }
    struct hw_device_t *dev = NULL;
    int rc = m->methods->open(m, "vk0", &dev);
    if (rc != 0 || !dev) {
        fail_why("HAL open(vk0) of %s failed: %d", g_be.hal_path, rc);
        return -1;
    }
    g_hal = (hwvulkan_device_t *)dev;
    LOGD("HAL device tag 0x%x version 0x%x", dev->tag, dev->version);
    bionic_environ_fixup();
    g_be.gipa = g_hal->GetInstanceProcAddr;
    g_be.enum_inst_ext = g_hal->EnumerateInstanceExtensionProperties;
    g_be.create_instance = g_hal->CreateInstance;
    g_be.enum_inst_version = (PFN_vkEnumerateInstanceVersion)g_be.gipa(NULL, "vkEnumerateInstanceVersion");
    g_be.kind = 1;
    return 0;
}
#endif

static int backend_init_glibc(void)
{
    const char *path = getenv("PD2_HYBRIS_DESK_ICD");
    if (!path || !*path) {
        LOGE("PD2_HYBRIS_BACKEND=glibc needs PD2_HYBRIS_DESK_ICD");
        return -1;
    }
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        LOGE("dlopen(%s): %s", path, dlerror());
        return -1;
    }
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(h, "vk_icdGetInstanceProcAddr");
    if (!gipa)
        gipa = (PFN_vkGetInstanceProcAddr)dlsym(h, "vkGetInstanceProcAddr");
    if (!gipa) {
        LOGE("%s has no vk_icdGetInstanceProcAddr", path);
        return -1;
    }
    typedef VkResult (*PFN_negotiate)(uint32_t *);
    PFN_negotiate neg = (PFN_negotiate)dlsym(h, "vk_icdNegotiateLoaderICDInterfaceVersion");
    if (neg) {
        uint32_t v = 5;
        neg(&v);
    }
    snprintf(g_be.hal_path, sizeof g_be.hal_path, "%s", path);
    g_be.gipa = gipa;
    g_be.enum_inst_ext = (PFN_vkEnumerateInstanceExtensionProperties)gipa(NULL, "vkEnumerateInstanceExtensionProperties");
    g_be.create_instance = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
    g_be.enum_inst_version = (PFN_vkEnumerateInstanceVersion)gipa(NULL, "vkEnumerateInstanceVersion");
    g_be.kind = 2;
    return 0;
}

/* PD2_HYBRIS_TEST_FAULT: a write to this address, a fault in the ICD's own code (tests only). */
static volatile uintptr_t g_test_fault_addr = 8;

static void backend_init_once(void)
{
    const char *b = getenv("PD2_HYBRIS_BACKEND");
    int rc;
    fault_watch_install();
    fault_report_install();
#ifdef HAVE_HYBRIS
    /* The loader dlcloses ICDs; bionic code keeps pointers into libhybris (pthread key destructors,
     * atexit handlers), so libhybris and this ICD stay mapped (this ICD is linked -z nodelete). */
    if (!dlopen("libhybris-common.so.1", RTLD_NOW | RTLD_NOLOAD | RTLD_NODELETE))
        LOGE("could not pin libhybris-common.so.1: %s", dlerror());
#endif
#ifdef HAVE_HYBRIS
    if (getenv("PD2_HYBRIS_TEST_FAIL")) { /* the app's fallback without a broken driver (tests only) */
        fail_why("the phone's Vulkan driver did not load: PD2_HYBRIS_TEST_FAIL");
        rc = -1;
    } else if (b && strcmp(b, "glibc") == 0)
        rc = backend_init_glibc();
    else
        rc = backend_init_hybris();
#else
    (void)b;
    rc = backend_init_glibc();
#endif
    g_be_ok = rc == 0 && g_be.gipa && g_be.create_instance && g_be.enum_inst_ext;
    fault_watch_refresh(); /* the HAL's libraries are in now */
    if (!g_be_ok) {
        LOGE("backend not available");
        status_line("failed %s", g_be_why[0] ? g_be_why : "the phone's Vulkan driver did not start");
    }
}

static int backend(void)
{
    pthread_once(&g_be_once, backend_init_once);
    return g_be_ok;
}

/* ------------------------------------------------------------------ bookkeeping */

struct inst {
    VkInstance h;
    struct inst *next;
    PFN_vkDestroyInstance DestroyInstance;
    PFN_vkEnumeratePhysicalDevices EnumeratePhysicalDevices;
    PFN_vkEnumeratePhysicalDeviceGroups EnumeratePhysicalDeviceGroups;
    PFN_vkEnumerateDeviceExtensionProperties EnumerateDeviceExtensionProperties;
    PFN_vkCreateDevice CreateDevice;
    PFN_vkGetPhysicalDeviceQueueFamilyProperties GetPhysicalDeviceQueueFamilyProperties;
    PFN_vkGetPhysicalDeviceMemoryProperties GetPhysicalDeviceMemoryProperties;
    PFN_vkGetPhysicalDeviceFormatProperties GetPhysicalDeviceFormatProperties;
    PFN_vkGetPhysicalDeviceImageFormatProperties GetPhysicalDeviceImageFormatProperties;
    PFN_vkGetDeviceProcAddr GetDeviceProcAddr;
};

struct phys {
    VkPhysicalDevice h;
    struct inst *inst;
    struct phys *next;
};

struct dev {
    VkDevice h;
    VkPhysicalDevice phys;
    struct inst *inst;
    struct dev *next;
    pthread_mutex_t qlock; /* one lock for every queue of the device (our submits race Zink's) */
    VkQueue queue;
    uint32_t qfam;
    VkPhysicalDeviceMemoryProperties mem;
#define DFN(n) PFN_vk##n n
    DFN(GetDeviceProcAddr);
    DFN(DestroyDevice);
    DFN(GetDeviceQueue);
    DFN(QueueSubmit);
    DFN(QueueSubmit2);
    DFN(QueueWaitIdle);
    DFN(QueueBindSparse);
    DFN(DeviceWaitIdle);
    DFN(CreateImage);
    DFN(DestroyImage);
    DFN(GetImageMemoryRequirements);
    DFN(AllocateMemory);
    DFN(FreeMemory);
    DFN(BindImageMemory);
    DFN(CreateBuffer);
    DFN(DestroyBuffer);
    DFN(GetBufferMemoryRequirements);
    DFN(BindBufferMemory);
    DFN(MapMemory);
    DFN(UnmapMemory);
    DFN(InvalidateMappedMemoryRanges);
    DFN(CreateCommandPool);
    DFN(DestroyCommandPool);
    DFN(AllocateCommandBuffers);
    DFN(BeginCommandBuffer);
    DFN(EndCommandBuffer);
    DFN(CmdPipelineBarrier);
    DFN(CmdCopyImageToBuffer);
    DFN(CmdBlitImage);
    DFN(CreateFence);
    DFN(DestroyFence);
    DFN(WaitForFences);
    DFN(ResetFences);
    DFN(GetAndroidHardwareBufferPropertiesANDROID);
    DFN(CreateGraphicsPipelines);
    DFN(CreateShaderModule);
#undef DFN
    int ahb;     /* VK_ANDROID_external_memory_android_hardware_buffer enabled: the direct present can import */
    int foreign; /* VK_EXT_queue_family_foreign enabled: the app's buffers go to the compositor's queue */
};

static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static struct inst *g_insts;
static struct phys *g_physs;
static struct dev *g_devs;

static struct phys *phys_get(VkPhysicalDevice h)
{
    pthread_mutex_lock(&g_lock);
    struct phys *p = g_physs;
    while (p && p->h != h)
        p = p->next;
    pthread_mutex_unlock(&g_lock);
    return p;
}

static void phys_add(VkPhysicalDevice h, struct inst *in)
{
    if (phys_get(h))
        return;
    struct phys *p = calloc(1, sizeof *p);
    p->h = h;
    p->inst = in;
    pthread_mutex_lock(&g_lock);
    p->next = g_physs;
    g_physs = p;
    pthread_mutex_unlock(&g_lock);
}

static struct inst *inst_get(VkInstance h)
{
    pthread_mutex_lock(&g_lock);
    struct inst *i = g_insts;
    while (i && i->h != h)
        i = i->next;
    pthread_mutex_unlock(&g_lock);
    return i;
}

static struct dev *dev_get(VkDevice h)
{
    pthread_mutex_lock(&g_lock);
    struct dev *d = g_devs;
    while (d && d->h != h)
        d = d->next;
    pthread_mutex_unlock(&g_lock);
    return d;
}

/* ------------------------------------------------------------------ extension lists */

static const char *const k_hidden_inst_ext[] = {
    "VK_KHR_surface", "VK_KHR_android_surface", "VK_KHR_xlib_surface", "VK_KHR_xcb_surface",
    "VK_KHR_wayland_surface", "VK_KHR_display", "VK_KHR_get_display_properties2",
    "VK_KHR_get_surface_capabilities2", "VK_EXT_surface_maintenance1", "VK_KHR_surface_maintenance1",
    "VK_EXT_swapchain_colorspace", "VK_KHR_surface_protected_capabilities", "VK_EXT_headless_surface",
    "VK_EXT_acquire_xlib_display", "VK_EXT_direct_mode_display", "VK_EXT_display_surface_counter",
    "VK_EXT_acquire_drm_display", "VK_KHR_portability_enumeration", "VK_GOOGLE_surfaceless_query",
};
static const VkExtensionProperties k_our_inst_ext[] = {
    { "VK_KHR_surface", 25 },
    { "VK_KHR_xlib_surface", 6 },
    { "VK_KHR_xcb_surface", 6 },
};
static const char *const k_hidden_dev_ext[] = {
    "VK_KHR_swapchain", "VK_KHR_swapchain_mutable_format", "VK_EXT_swapchain_maintenance1",
    "VK_KHR_swapchain_maintenance1", "VK_KHR_incremental_present", "VK_GOOGLE_display_timing",
    "VK_KHR_present_id", "VK_KHR_present_wait", "VK_KHR_present_id2", "VK_KHR_present_wait2",
    "VK_EXT_display_control", "VK_ANDROID_native_buffer", "VK_KHR_shared_presentable_image",
    "VK_EXT_full_screen_exclusive", "VK_EXT_hdr_metadata", "VK_EXT_present_mode_fifo_latest_ready",
    "VK_KHR_present_mode_fifo_latest_ready", "VK_ANDROID_frontbuffer_rendering",
};
static const VkExtensionProperties k_our_dev_ext[] = {
    { "VK_KHR_swapchain", 70 },
};
/* Enabled on the phone's driver for the direct present (pd2_CreateDevice): the AHardwareBuffer
 * import first, then the compositor's queue family. */
static const char *const k_direct_dev_ext[] = {
    "VK_ANDROID_external_memory_android_hardware_buffer",
    "VK_EXT_queue_family_foreign",
};

static int name_in(const char *n, const char *const *list, size_t len)
{
    for (size_t i = 0; i < len; i++)
        if (strcmp(n, list[i]) == 0)
            return 1;
    return 0;
}

/* PD2_HYBRIS_HIDE_EXT: device extensions to hide too, comma-separated (tests: which of the
 * driver's extensions a Zink path needs). */
static int hidden_by_test(const char *n)
{
    const char *list = getenv("PD2_HYBRIS_HIDE_EXT");
    size_t len = strlen(n);
    for (const char *p = list; p && *p;) {
        const char *e = strchr(p, ',');
        size_t l = e ? (size_t)(e - p) : strlen(p);
        if (l == len && strncmp(p, n, len) == 0)
            return 1;
        p = e ? e + 1 : NULL;
    }
    return 0;
}

/* Merge the backend's list (minus hidden names) with ours, Vulkan's two-call idiom. */
static VkResult merge_ext(VkExtensionProperties *be, uint32_t be_n, const char *const *hidden, size_t hidden_n,
                          const VkExtensionProperties *ours, size_t ours_n, uint32_t *count, VkExtensionProperties *out)
{
    VkExtensionProperties *all = calloc(be_n + ours_n + 1, sizeof *all);
    uint32_t n = 0;
    for (uint32_t i = 0; i < be_n; i++)
        if (!name_in(be[i].extensionName, hidden, hidden_n) && !hidden_by_test(be[i].extensionName))
            all[n++] = be[i];
    for (size_t i = 0; i < ours_n; i++)
        all[n++] = ours[i];
    VkResult r = VK_SUCCESS;
    if (!out) {
        *count = n;
    } else {
        uint32_t c = *count < n ? *count : n;
        memcpy(out, all, c * sizeof *all);
        if (c < n)
            r = VK_INCOMPLETE;
        *count = c;
    }
    free(all);
    return r;
}

/* ------------------------------------------------------------------ global commands */

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumerateInstanceExtensionProperties(const char *layer, uint32_t *count,
                                                                             VkExtensionProperties *props)
{
    if (layer)
        return VK_ERROR_LAYER_NOT_PRESENT;
    if (!backend())
        return VK_ERROR_INITIALIZATION_FAILED;
    uint32_t n = 0;
    VkResult r = g_be.enum_inst_ext(NULL, &n, NULL);
    if (r != VK_SUCCESS)
        return r;
    VkExtensionProperties *be = calloc(n + 1, sizeof *be);
    r = g_be.enum_inst_ext(NULL, &n, be);
    if (r < 0) {
        free(be);
        return r;
    }
    r = merge_ext(be, n, k_hidden_inst_ext, ARRAY_LEN(k_hidden_inst_ext), k_our_inst_ext, ARRAY_LEN(k_our_inst_ext),
                  count, props);
    free(be);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumerateInstanceLayerProperties(uint32_t *count, VkLayerProperties *props)
{
    (void)props;
    *count = 0;
    return VK_SUCCESS;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumerateInstanceVersion(uint32_t *v)
{
    if (!backend())
        return VK_ERROR_INITIALIZATION_FAILED;
    if (g_be.enum_inst_version)
        return g_be.enum_inst_version(v);
    *v = VK_API_VERSION_1_0;
    return VK_SUCCESS;
}

#define IGIPA(in, n) in->n = (PFN_vk##n)g_be.gipa(in->h, "vk" #n)

static VKAPI_ATTR VkResult VKAPI_CALL pd2_CreateInstance(const VkInstanceCreateInfo *ci, const VkAllocationCallbacks *a,
                                                       VkInstance *out)
{
    if (!backend())
        return VK_ERROR_INITIALIZATION_FAILED;
    /* The app's fallback after a crash in the route (tests only): a real fault in this ICD's code,
     * which the fault watch (installed by backend()) reports before Wine gets it, as a crash in
     * the phone's driver. Here, not in the backend's pthread_once: a once that Wine's exception
     * leaves unfinished would hold every later call of the process. */
    if (getenv("PD2_HYBRIS_TEST_FAULT"))
        *(volatile int *)g_test_fault_addr = 1;
    VkInstanceCreateInfo c = *ci;
    const char **names = calloc(ci->enabledExtensionCount + 1, sizeof *names);
    uint32_t n = 0;
    for (uint32_t i = 0; i < ci->enabledExtensionCount; i++) {
        const char *e = ci->ppEnabledExtensionNames[i];
        int ours = 0;
        for (size_t k = 0; k < ARRAY_LEN(k_our_inst_ext); k++)
            if (strcmp(e, k_our_inst_ext[k].extensionName) == 0)
                ours = 1;
        if (!ours)
            names[n++] = e;
        LOGD("instance extension %s%s", e, ours ? " (ours)" : "");
    }
    c.enabledExtensionCount = n;
    c.ppEnabledExtensionNames = names;
    c.enabledLayerCount = 0;
    c.ppEnabledLayerNames = NULL;
    VkResult r = g_be.create_instance(&c, a, out);
    free(names);
    LOGD("CreateInstance: %d (api 0x%x)", r, ci->pApplicationInfo ? ci->pApplicationInfo->apiVersion : 0);
    if (r != VK_SUCCESS) {
        status_line("failed vkCreateInstance of the phone's Vulkan driver: %d", r);
        return r;
    }
    if (g_be.kind == 1) {
        uintptr_t magic = *(uintptr_t *)*out;
        if (magic != HWVULKAN_DISPATCH_MAGIC)
            LOGD("instance dispatch word 0x%" PRIxPTR " (not the HAL magic)", magic);
    }
    struct inst *in = calloc(1, sizeof *in);
    in->h = *out;
    IGIPA(in, DestroyInstance);
    IGIPA(in, EnumeratePhysicalDevices);
    IGIPA(in, EnumeratePhysicalDeviceGroups);
    IGIPA(in, EnumerateDeviceExtensionProperties);
    IGIPA(in, CreateDevice);
    IGIPA(in, GetPhysicalDeviceQueueFamilyProperties);
    IGIPA(in, GetPhysicalDeviceMemoryProperties);
    IGIPA(in, GetPhysicalDeviceFormatProperties);
    IGIPA(in, GetPhysicalDeviceImageFormatProperties);
    IGIPA(in, GetDeviceProcAddr);
    pthread_mutex_lock(&g_lock);
    in->next = g_insts;
    g_insts = in;
    pthread_mutex_unlock(&g_lock);
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL pd2_DestroyInstance(VkInstance h, const VkAllocationCallbacks *a)
{
    struct inst *in = inst_get(h);
    if (!in)
        return;
    pthread_mutex_lock(&g_lock);
    struct inst **pp = &g_insts;
    while (*pp && *pp != in)
        pp = &(*pp)->next;
    if (*pp)
        *pp = in->next;
    struct phys **pq = &g_physs;
    while (*pq) {
        if ((*pq)->inst == in) {
            struct phys *x = *pq;
            *pq = x->next;
            free(x);
        } else {
            pq = &(*pq)->next;
        }
    }
    pthread_mutex_unlock(&g_lock);
    in->DestroyInstance(h, a);
    free(in);
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumeratePhysicalDevices(VkInstance h, uint32_t *count, VkPhysicalDevice *pd)
{
    struct inst *in = inst_get(h);
    if (!in)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = in->EnumeratePhysicalDevices(h, count, pd);
    if (pd && r >= 0)
        for (uint32_t i = 0; i < *count; i++)
            phys_add(pd[i], in);
    LOGD("EnumeratePhysicalDevices: %d, %u", r, *count);
    if (!pd && (r < 0 || *count == 0))
        status_line("failed the phone's Vulkan driver has no device: %d", r);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumeratePhysicalDeviceGroups(VkInstance h, uint32_t *count,
                                                                      VkPhysicalDeviceGroupProperties *g)
{
    struct inst *in = inst_get(h);
    if (!in || !in->EnumeratePhysicalDeviceGroups)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkResult r = in->EnumeratePhysicalDeviceGroups(h, count, g);
    if (g && r >= 0)
        for (uint32_t i = 0; i < *count; i++)
            for (uint32_t k = 0; k < g[i].physicalDeviceCount; k++)
                phys_add(g[i].physicalDevices[k], in);
    return r;
}

static struct inst *inst_of(VkPhysicalDevice p)
{
    struct phys *ph = phys_get(p);
    if (ph)
        return ph->inst;
    /* A physical device that came through another path: one instance is the usual case. */
    pthread_mutex_lock(&g_lock);
    struct inst *in = g_insts;
    pthread_mutex_unlock(&g_lock);
    return in;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_EnumerateDeviceExtensionProperties(VkPhysicalDevice p, const char *layer,
                                                                           uint32_t *count, VkExtensionProperties *props)
{
    struct inst *in = inst_of(p);
    if (!in)
        return VK_ERROR_INITIALIZATION_FAILED;
    if (layer)
        return in->EnumerateDeviceExtensionProperties(p, layer, count, props);
    uint32_t n = 0;
    VkResult r = in->EnumerateDeviceExtensionProperties(p, NULL, &n, NULL);
    if (r != VK_SUCCESS)
        return r;
    VkExtensionProperties *be = calloc(n + 1, sizeof *be);
    r = in->EnumerateDeviceExtensionProperties(p, NULL, &n, be);
    if (r < 0) {
        free(be);
        return r;
    }
    r = merge_ext(be, n, k_hidden_dev_ext, ARRAY_LEN(k_hidden_dev_ext), k_our_dev_ext, ARRAY_LEN(k_our_dev_ext), count,
                  props);
    free(be);
    return r;
}

#define DGDPA(d, n) d->n = (PFN_vk##n)d->GetDeviceProcAddr(d->h, "vk" #n)

static VKAPI_ATTR VkResult VKAPI_CALL pd2_CreateDevice(VkPhysicalDevice p, const VkDeviceCreateInfo *ci,
                                                     const VkAllocationCallbacks *a, VkDevice *out)
{
    struct inst *in = inst_of(p);
    if (!in)
        return VK_ERROR_INITIALIZATION_FAILED;
    VkDeviceCreateInfo c = *ci;
    const char **names = calloc(ci->enabledExtensionCount + ARRAY_LEN(k_direct_dev_ext) + 1, sizeof *names);
    uint32_t n = 0;
    for (uint32_t i = 0; i < ci->enabledExtensionCount; i++) {
        const char *e = ci->ppEnabledExtensionNames[i];
        if (!name_in(e, k_hidden_dev_ext, ARRAY_LEN(k_hidden_dev_ext)) && !hidden_by_test(e))
            names[n++] = e;
    }
    /* The direct present imports the app's AHardwareBuffers (the phone's driver only): the
     * extensions it needs, where the driver has them and the app did not ask for them. */
    uint32_t asked_n = n;
    int direct_ext = 0;
    if (g_be.kind == 1) {
        uint32_t have_n = 0;
        in->EnumerateDeviceExtensionProperties(p, NULL, &have_n, NULL);
        VkExtensionProperties *have = calloc(have_n + 1, sizeof *have);
        in->EnumerateDeviceExtensionProperties(p, NULL, &have_n, have);
        for (uint32_t i = 0; i < have_n; i++)
            LOGD("the driver's device extension %s %u", have[i].extensionName, have[i].specVersion);
        int has_all = 1;
        for (size_t k = 0; k < ARRAY_LEN(k_direct_dev_ext); k++) {
            int has = 0;
            for (uint32_t i = 0; i < have_n; i++)
                has |= strcmp(have[i].extensionName, k_direct_dev_ext[k]) == 0;
            has_all &= has;
        }
        free(have);
        for (size_t k = 0; has_all && k < ARRAY_LEN(k_direct_dev_ext); k++) {
            int asked = 0;
            for (uint32_t i = 0; i < asked_n; i++)
                asked |= strcmp(names[i], k_direct_dev_ext[k]) == 0;
            if (!asked)
                names[n++] = k_direct_dev_ext[k];
        }
        direct_ext = has_all;
    }
    c.enabledExtensionCount = n;
    c.ppEnabledExtensionNames = names;
    c.enabledLayerCount = 0;
    c.ppEnabledLayerNames = NULL;
    VkResult r = in->CreateDevice(p, &c, a, out);
    if (r != VK_SUCCESS && n > asked_n) {
        /* Never fail the app's device for the direct present's extensions. */
        LOGE("CreateDevice with the direct present's extensions: %d; again without them", r);
        c.enabledExtensionCount = asked_n;
        direct_ext = 0;
        r = in->CreateDevice(p, &c, a, out);
    }
    free(names);
    LOGD("CreateDevice: %d (%u extensions passed on)", r, n);
    {
        VkPhysicalDeviceProperties pp;
        PFN_vkGetPhysicalDeviceProperties props =
            (PFN_vkGetPhysicalDeviceProperties)g_be.gipa(in->h, "vkGetPhysicalDeviceProperties");
        memset(&pp, 0, sizeof pp);
        if (props)
            props(p, &pp);
        if (r == VK_SUCCESS)
            status_line("ok %s, driver 0x%x, %s", pp.deviceName, pp.driverVersion, g_be.hal_path);
        else
            status_line("failed vkCreateDevice of the phone's Vulkan driver (%s): %d", pp.deviceName, r);
    }
    if (r != VK_SUCCESS)
        return r;
    struct dev *d = calloc(1, sizeof *d);
    d->h = *out;
    d->phys = p;
    d->inst = in;
    pthread_mutex_init(&d->qlock, NULL);
    d->GetDeviceProcAddr = in->GetDeviceProcAddr;
    DGDPA(d, DestroyDevice);
    DGDPA(d, GetDeviceQueue);
    DGDPA(d, QueueSubmit);
    DGDPA(d, QueueSubmit2);
    DGDPA(d, QueueWaitIdle);
    DGDPA(d, QueueBindSparse);
    DGDPA(d, DeviceWaitIdle);
    DGDPA(d, CreateImage);
    DGDPA(d, DestroyImage);
    DGDPA(d, GetImageMemoryRequirements);
    DGDPA(d, AllocateMemory);
    DGDPA(d, FreeMemory);
    DGDPA(d, BindImageMemory);
    DGDPA(d, CreateBuffer);
    DGDPA(d, DestroyBuffer);
    DGDPA(d, GetBufferMemoryRequirements);
    DGDPA(d, BindBufferMemory);
    DGDPA(d, MapMemory);
    DGDPA(d, UnmapMemory);
    DGDPA(d, InvalidateMappedMemoryRanges);
    DGDPA(d, CreateCommandPool);
    DGDPA(d, DestroyCommandPool);
    DGDPA(d, AllocateCommandBuffers);
    DGDPA(d, BeginCommandBuffer);
    DGDPA(d, EndCommandBuffer);
    DGDPA(d, CmdPipelineBarrier);
    DGDPA(d, CmdCopyImageToBuffer);
    DGDPA(d, CmdBlitImage);
    DGDPA(d, CreateFence);
    DGDPA(d, DestroyFence);
    DGDPA(d, WaitForFences);
    DGDPA(d, ResetFences);
    DGDPA(d, CreateGraphicsPipelines);
    DGDPA(d, CreateShaderModule);
    if (direct_ext)
        DGDPA(d, GetAndroidHardwareBufferPropertiesANDROID);
    d->ahb = direct_ext && d->GetAndroidHardwareBufferPropertiesANDROID && d->CmdBlitImage;
    d->foreign = direct_ext;
    fault_watch_refresh();
    if (!d->QueueSubmit2)
        d->QueueSubmit2 = (PFN_vkQueueSubmit2)d->GetDeviceProcAddr(d->h, "vkQueueSubmit2KHR");
    in->GetPhysicalDeviceMemoryProperties(p, &d->mem);
    /* Our internal queue: the first queue of the first family that the app asked for. */
    d->qfam = ci->queueCreateInfoCount ? ci->pQueueCreateInfos[0].queueFamilyIndex : 0;
    d->GetDeviceQueue(d->h, d->qfam, 0, &d->queue);
    pthread_mutex_lock(&g_lock);
    d->next = g_devs;
    g_devs = d;
    pthread_mutex_unlock(&g_lock);
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL pd2_DestroyDevice(VkDevice h, const VkAllocationCallbacks *a)
{
    struct dev *d = dev_get(h);
    if (!d)
        return;
    pthread_mutex_lock(&g_lock);
    struct dev **pp = &g_devs;
    while (*pp && *pp != d)
        pp = &(*pp)->next;
    if (*pp)
        *pp = d->next;
    pthread_mutex_unlock(&g_lock);
    d->DestroyDevice(h, a);
    pthread_mutex_destroy(&d->qlock);
    free(d);
}

/* ------------------------------------------------------------------ queue lock */

/* Queues are looked up by device: Vulkan gives no queue -> device call, so the queue's device is
 * remembered when the app asks for it. */
struct qmap {
    VkQueue q;
    struct dev *d;
    struct qmap *next;
};
static struct qmap *g_qmap;

static struct dev *dev_of_queue(VkQueue q)
{
    pthread_mutex_lock(&g_lock);
    struct qmap *m = g_qmap;
    while (m && m->q != q)
        m = m->next;
    struct dev *d = m ? m->d : NULL;
    if (!d && g_devs && !g_devs->next)
        d = g_devs;
    pthread_mutex_unlock(&g_lock);
    return d;
}

static void qmap_add(VkQueue q, struct dev *d)
{
    pthread_mutex_lock(&g_lock);
    struct qmap *m = g_qmap;
    while (m && m->q != q)
        m = m->next;
    if (!m) {
        m = calloc(1, sizeof *m);
        m->q = q;
        m->next = g_qmap;
        g_qmap = m;
    }
    m->d = d;
    pthread_mutex_unlock(&g_lock);
}

static VKAPI_ATTR void VKAPI_CALL pd2_GetDeviceQueue(VkDevice h, uint32_t fam, uint32_t idx, VkQueue *q)
{
    struct dev *d = dev_get(h);
    d->GetDeviceQueue(h, fam, idx, q);
    qmap_add(*q, d);
}

static VKAPI_ATTR void VKAPI_CALL pd2_GetDeviceQueue2(VkDevice h, const VkDeviceQueueInfo2 *info, VkQueue *q)
{
    struct dev *d = dev_get(h);
    PFN_vkGetDeviceQueue2 f = (PFN_vkGetDeviceQueue2)d->GetDeviceProcAddr(h, "vkGetDeviceQueue2");
    f(h, info, q);
    qmap_add(*q, d);
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_QueueSubmit(VkQueue q, uint32_t n, const VkSubmitInfo *s, VkFence f)
{
    struct dev *d = dev_of_queue(q);
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->QueueSubmit(q, n, s, f);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_QueueSubmit2(VkQueue q, uint32_t n, const VkSubmitInfo2 *s, VkFence f)
{
    struct dev *d = dev_of_queue(q);
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->QueueSubmit2(q, n, s, f);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_QueueWaitIdle(VkQueue q)
{
    struct dev *d = dev_of_queue(q);
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->QueueWaitIdle(q);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_QueueBindSparse(VkQueue q, uint32_t n, const VkBindSparseInfo *b, VkFence f)
{
    struct dev *d = dev_of_queue(q);
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->QueueBindSparse(q, n, b, f);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_DeviceWaitIdle(VkDevice h)
{
    struct dev *d = dev_get(h);
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->DeviceWaitIdle(h);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

/* ------------------------------------------------------------------ surfaces */

struct xwin {
    xcb_connection_t *c;
    xcb_window_t w;
};

static int surface_xwin(VkSurfaceKHR s, struct xwin *x)
{
    VkIcdSurfaceBase *b = (VkIcdSurfaceBase *)(uintptr_t)s;
    if (!b)
        return -1;
    if (b->platform == VK_ICD_WSI_PLATFORM_XCB) {
        VkIcdSurfaceXcb *xs = (VkIcdSurfaceXcb *)b;
        x->c = xs->connection;
        x->w = xs->window;
        return 0;
    }
    if (b->platform == VK_ICD_WSI_PLATFORM_XLIB) {
        VkIcdSurfaceXlib *xs = (VkIcdSurfaceXlib *)b;
        x->c = XGetXCBConnection(xs->dpy);
        x->w = (xcb_window_t)xs->window;
        return 0;
    }
    LOGE("surface platform %d is not X11", b->platform);
    return -1;
}

static int window_geometry(struct xwin *x, uint32_t *w, uint32_t *h, uint8_t *depth)
{
    xcb_get_geometry_reply_t *g = xcb_get_geometry_reply(x->c, xcb_get_geometry(x->c, x->w), NULL);
    if (!g)
        return -1;
    *w = g->width;
    *h = g->height;
    if (depth)
        *depth = g->depth;
    free(g);
    return 0;
}

static VKAPI_ATTR void VKAPI_CALL pd2_DestroySurfaceKHR(VkInstance i, VkSurfaceKHR s, const VkAllocationCallbacks *a)
{
    (void)i;
    (void)s;
    (void)a; /* loader-created surfaces belong to the loader */
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetPhysicalDeviceSurfaceSupportKHR(VkPhysicalDevice p, uint32_t fam,
                                                                           VkSurfaceKHR s, VkBool32 *ok)
{
    struct inst *in = inst_of(p);
    uint32_t n = 0;
    in->GetPhysicalDeviceQueueFamilyProperties(p, &n, NULL);
    VkQueueFamilyProperties *qf = calloc(n + 1, sizeof *qf);
    in->GetPhysicalDeviceQueueFamilyProperties(p, &n, qf);
    *ok = fam < n && (qf[fam].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_TRANSFER_BIT | VK_QUEUE_COMPUTE_BIT));
    free(qf);
    struct xwin x;
    if (surface_xwin(s, &x) != 0)
        *ok = VK_FALSE;
    return VK_SUCCESS;
}

static VKAPI_ATTR VkBool32 VKAPI_CALL pd2_GetPhysicalDeviceXcbPresentationSupportKHR(VkPhysicalDevice p, uint32_t fam,
                                                                                   xcb_connection_t *c, xcb_visualid_t v)
{
    (void)p;
    (void)fam;
    (void)c;
    (void)v;
    return VK_TRUE;
}

static VKAPI_ATTR VkBool32 VKAPI_CALL pd2_GetPhysicalDeviceXlibPresentationSupportKHR(VkPhysicalDevice p, uint32_t fam,
                                                                                    Display *dpy, VisualID v)
{
    (void)p;
    (void)fam;
    (void)dpy;
    (void)v;
    return VK_TRUE;
}

#define SURFACE_USAGE                                                                                          \
    (VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | \
     VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT)
#define MAX_IMAGES 8

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetPhysicalDeviceSurfaceCapabilitiesKHR(VkPhysicalDevice p, VkSurfaceKHR s,
                                                                                VkSurfaceCapabilitiesKHR *caps)
{
    (void)p;
    struct xwin x;
    uint32_t w = 0, h = 0;
    if (surface_xwin(s, &x) != 0 || window_geometry(&x, &w, &h, NULL) != 0)
        return VK_ERROR_SURFACE_LOST_KHR;
    memset(caps, 0, sizeof *caps);
    caps->minImageCount = 2;
    caps->maxImageCount = MAX_IMAGES;
    caps->currentExtent = (VkExtent2D){ w, h };
    caps->minImageExtent = caps->currentExtent;
    caps->maxImageExtent = caps->currentExtent;
    caps->maxImageArrayLayers = 1;
    caps->supportedTransforms = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR;
    caps->currentTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR;
    caps->supportedCompositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR | VK_COMPOSITE_ALPHA_INHERIT_BIT_KHR;
    caps->supportedUsageFlags = SURFACE_USAGE;
    return VK_SUCCESS;
}

/* BGRA first: X's 24/32-bit visuals take B,G,R,X bytes, so those copy with no swizzle. */
static const VkFormat k_formats[] = { VK_FORMAT_B8G8R8A8_UNORM, VK_FORMAT_B8G8R8A8_SRGB, VK_FORMAT_R8G8B8A8_UNORM,
                                      VK_FORMAT_R8G8B8A8_SRGB };

static int format_ok(struct inst *in, VkPhysicalDevice p, VkFormat f)
{
    VkFormatProperties fp;
    in->GetPhysicalDeviceFormatProperties(p, f, &fp);
    VkFormatFeatureFlags need = VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT | VK_FORMAT_FEATURE_TRANSFER_SRC_BIT;
    return (fp.optimalTilingFeatures & need) == need;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetPhysicalDeviceSurfaceFormatsKHR(VkPhysicalDevice p, VkSurfaceKHR s,
                                                                           uint32_t *count, VkSurfaceFormatKHR *f)
{
    (void)s;
    struct inst *in = inst_of(p);
    VkSurfaceFormatKHR all[ARRAY_LEN(k_formats)];
    uint32_t n = 0;
    for (size_t i = 0; i < ARRAY_LEN(k_formats); i++)
        if (format_ok(in, p, k_formats[i]))
            all[n++] = (VkSurfaceFormatKHR){ k_formats[i], VK_COLOR_SPACE_SRGB_NONLINEAR_KHR };
    if (!f) {
        *count = n;
        return VK_SUCCESS;
    }
    uint32_t c = *count < n ? *count : n;
    memcpy(f, all, c * sizeof *f);
    *count = c;
    return c < n ? VK_INCOMPLETE : VK_SUCCESS;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetPhysicalDeviceSurfacePresentModesKHR(VkPhysicalDevice p, VkSurfaceKHR s,
                                                                                uint32_t *count, VkPresentModeKHR *m)
{
    (void)p;
    (void)s;
    /* Every mode behaves the same: each present waits for the app's clock on the present thread
     * (present_counter.h); FIFO is required by the spec. */
    static const VkPresentModeKHR modes[] = { VK_PRESENT_MODE_FIFO_KHR, VK_PRESENT_MODE_IMMEDIATE_KHR,
                                              VK_PRESENT_MODE_MAILBOX_KHR };
    uint32_t n = ARRAY_LEN(modes);
    if (!m) {
        *count = n;
        return VK_SUCCESS;
    }
    uint32_t c = *count < n ? *count : n;
    memcpy(m, modes, c * sizeof *m);
    *count = c;
    return c < n ? VK_INCOMPLETE : VK_SUCCESS;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetPhysicalDevicePresentRectanglesKHR(VkPhysicalDevice p, VkSurfaceKHR s,
                                                                              uint32_t *count, VkRect2D *r)
{
    (void)p;
    struct xwin x;
    uint32_t w = 0, h = 0;
    if (surface_xwin(s, &x) != 0 || window_geometry(&x, &w, &h, NULL) != 0)
        return VK_ERROR_SURFACE_LOST_KHR;
    if (!r) {
        *count = 1;
        return VK_SUCCESS;
    }
    if (*count < 1)
        return VK_INCOMPLETE;
    r[0] = (VkRect2D){ { 0, 0 }, { w, h } };
    *count = 1;
    return VK_SUCCESS;
}

/* ------------------------------------------------------------------ swapchain */

/* The swapchain. Its images are the driver's own. vkQueuePresentKHR only submits the GPU work of a
 * present (the copy into a host-cached buffer for the X screen, or the blit into one of the app's
 * buffers, the direct present) with a fence, and queues the present; the swapchain's present
 * thread waits for the fence, for the app's clock and pacing (present_counter.h), and puts the
 * frame on the X screen or hands the app its buffer. So the game's thread never waits for the GPU
 * or the X server, as with Mesa's WSI (patches 0006 and 0004). An image can be acquired again once
 * its present is done. */
/* The app's buffers, at most. Mesa's SW_DIRECT_MAX_BUFFERS is 5 (patch 0008): with 3 images both
 * give 4 buffers, and with 4 images this swapchain has no spare buffer. */
#define DP_MAX 4

enum { IMG_FREE, IMG_ACQUIRED, IMG_QUEUED };

struct img {
    VkImage image;
    VkDeviceMemory mem;
    VkBuffer buf;
    VkDeviceMemory bufmem;
    uint8_t *map;
    VkCommandBuffer cmd;          /* the copy for the X screen */
    VkCommandBuffer dcmd[DP_MAX]; /* the blit into each of the app's buffers */
    VkFence fence;
    int state; /* IMG_*, under the swapchain's lock */
};

struct job {
    uint32_t index;
    int buffer; /* the app's buffer of the direct present, -1: the X screen */
    VkPresentModeKHR mode;
};

/* The direct present: the client of the app's present server (app/src/main/cpp/
 * present_server.cpp), with the protocol of Mesa patch 0004. */
enum { DP_FREE, DP_RESERVED, DP_HELD };
struct direct {
    int sock;
    uint32_t count;
    struct AHardwareBuffer *ahb[DP_MAX];
    VkImage image[DP_MAX];
    VkDeviceMemory mem[DP_MAX];
    int state[DP_MAX]; /* DP_*, under the swapchain's lock */
    int gone;          /* under the swapchain's lock: the app has gone; the X screen from now on */
    pthread_t reader;
    int reader_started;
};

struct swapchain {
    struct dev *d;
    struct xwin x;
    xcb_gcontext_t gc;
    uint8_t depth;
    VkExtent2D extent;
    VkFormat format;
    uint32_t count;
    uint32_t next;
    int coherent;
    int swizzle;
    VkDeviceSize bufsize;
    VkCommandPool pool;
    uint8_t *swz; /* swizzle row buffer for RGBA formats */
    uint64_t presents;
    VkPresentModeKHR mode;
    struct pc_clock clock; /* the app's clock as this swapchain last saw it (present_counter.h) */
    uint32_t clock_waiting; /* presented images that have not passed the clock, under [lock] (pc_clock_holds) */
    struct img img[MAX_IMAGES];
    /* The present thread and its queue, under [lock]; [cond] wakes the thread, an acquire that
     * waits for a free image, and a present that waits for a free buffer of the app. */
    pthread_mutex_t lock;
    pthread_cond_t cond;
    struct job q[MAX_IMAGES];
    uint32_t qhead, qn;
    int stop;
    pthread_t thread;
    int thread_started;
    struct direct *dir;
    uint64_t direct_presents;
};

static VkResult acquire_image(struct swapchain *sc, uint64_t timeout, uint32_t *index);
static void *present_thread(void *arg);

/* ------------------------------------------------------------------ the direct present */

#define DP_MAGIC 0x50443250u /* "P2DP" in memory */
#define DP_VERSION 1
#define DP_HELLO 1
#define DP_BUFFERS 2
#define DP_PRESENT 3
#define DP_RELEASE 4
#define DP_AHB_R8G8B8A8 1           /* AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM */
#define DP_WAIT_NS (100LL * 1000 * 1000)

struct dp_msg {
    uint32_t magic, version, type, index, width, height, count, format;
};

/* bionic's AHardwareBuffer calls (libnativewindow.so through libhybris): the app sends its buffers
 * with AHardwareBuffer_sendHandleToUnixSocket, and the phone's driver imports an AHardwareBuffer
 * (VK_ANDROID_external_memory_android_hardware_buffer), so no layout of Google's gralloc is ours
 * to know. */
typedef int (*PFN_ahb_recv)(int fd, struct AHardwareBuffer **out);
typedef void (*PFN_ahb_release)(struct AHardwareBuffer *b);
static PFN_ahb_recv g_ahb_recv;
static PFN_ahb_release g_ahb_release;
static pthread_once_t g_ahb_once = PTHREAD_ONCE_INIT;

static void ahb_load(void)
{
#ifdef HAVE_HYBRIS
    if (g_be.kind != 1)
        return;
    void *h = hybris_dlopen("libnativewindow.so", RTLD_NOW);
    if (!h) {
        LOGE("direct present: libnativewindow.so did not load: %s", hybris_dlerror());
        return;
    }
    g_ahb_recv = (PFN_ahb_recv)hybris_dlsym(h, "AHardwareBuffer_recvHandleFromUnixSocket");
    g_ahb_release = (PFN_ahb_release)hybris_dlsym(h, "AHardwareBuffer_release");
#endif
}

static int dp_send(int sock, uint32_t type, uint32_t index, uint32_t w, uint32_t h, uint32_t count)
{
    struct dp_msg m = { DP_MAGIC, DP_VERSION, type, index, w, h, count, 0 };
    ssize_t n;
    do {
        n = send(sock, &m, sizeof m, MSG_NOSIGNAL);
    } while (n < 0 && errno == EINTR);
    return n == (ssize_t)sizeof m;
}

/* One message into [m] and its descriptors into [fds] (the rest closed): its length, 0 at the
 * end, -1 on an error. */
static ssize_t dp_recv(int sock, struct dp_msg *m, int *fds, int max_fds, int *nfds)
{
    struct iovec iov = { m, sizeof *m };
    union {
        char buf[CMSG_SPACE(8 * sizeof(int))];
        struct cmsghdr align;
    } control;
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control.buf,
                          .msg_controllen = sizeof control.buf };
    ssize_t n;
    do {
        n = recvmsg(sock, &msg, MSG_CMSG_CLOEXEC);
    } while (n < 0 && errno == EINTR);
    *nfds = 0;
    if (n < 0)
        return -1;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c; c = CMSG_NXTHDR(&msg, c)) {
        if (c->cmsg_level != SOL_SOCKET || c->cmsg_type != SCM_RIGHTS)
            continue;
        int count = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
        int *got = (int *)CMSG_DATA(c);
        for (int i = 0; i < count; i++) {
            if (*nfds < max_fds)
                fds[(*nfds)++] = got[i];
            else
                close(got[i]);
        }
    }
    if (msg.msg_flags & (MSG_TRUNC | MSG_CTRUNC)) {
        for (int i = 0; i < *nfds; i++)
            close(fds[i]);
        *nfds = 0;
        return -1;
    }
    return n;
}

/* The app's buffer [b] is free again; a present that waits for one wakes. */
static void dp_release(struct swapchain *sc, uint32_t b)
{
    pthread_mutex_lock(&sc->lock);
    if (sc->dir->state[b] == DP_HELD)
        sc->dir->state[b] = DP_FREE;
    pthread_cond_broadcast(&sc->cond);
    pthread_mutex_unlock(&sc->lock);
}

/* The app's RELEASE messages: a buffer is free again once its release fence (a sync_file) has
 * signalled; the fences are polled beside the socket. At the end (the app has gone, or the
 * swapchain closes) the presents go to the X screen. */
static void *dp_reader(void *arg)
{
    struct swapchain *sc = arg;
    struct direct *dir = sc->dir;
    int fences[DP_MAX];
    for (int b = 0; b < DP_MAX; b++)
        fences[b] = -1;
    for (;;) {
        struct pollfd p[1 + DP_MAX];
        uint32_t owner[1 + DP_MAX];
        nfds_t np = 0;
        p[np++] = (struct pollfd){ dir->sock, POLLIN, 0 };
        for (uint32_t b = 0; b < dir->count; b++) {
            if (fences[b] >= 0) {
                owner[np] = b;
                p[np++] = (struct pollfd){ fences[b], POLLIN, 0 };
            }
        }
        if (poll(p, np, -1) < 0) {
            if (errno == EINTR)
                continue;
            break;
        }
        for (nfds_t i = 1; i < np; i++) {
            if (!p[i].revents)
                continue;
            close(fences[owner[i]]);
            fences[owner[i]] = -1;
            dp_release(sc, owner[i]);
        }
        if (!p[0].revents)
            continue;
        struct dp_msg m;
        int fds[4], nfds;
        ssize_t n = dp_recv(dir->sock, &m, fds, 4, &nfds);
        if (n <= 0)
            break;
        if (n != (ssize_t)sizeof m || m.magic != DP_MAGIC || m.type != DP_RELEASE || m.index >= dir->count) {
            for (int i = 0; i < nfds; i++)
                close(fds[i]);
            continue;
        }
        for (int i = 1; i < nfds; i++)
            close(fds[i]);
        pthread_mutex_lock(&sc->lock);
        int held = dir->state[m.index] == DP_HELD;
        pthread_mutex_unlock(&sc->lock);
        if (!held || nfds == 0) {
            if (nfds > 0)
                close(fds[0]);
            dp_release(sc, m.index);
            continue;
        }
        if (fences[m.index] >= 0)
            close(fences[m.index]);
        fences[m.index] = fds[0];
    }
    for (int b = 0; b < DP_MAX; b++)
        if (fences[b] >= 0)
            close(fences[b]);
    pthread_mutex_lock(&sc->lock);
    dir->gone = 1;
    pthread_cond_broadcast(&sc->cond);
    pthread_mutex_unlock(&sc->lock);
    shutdown(dir->sock, SHUT_RDWR);
    return NULL;
}

/* Imports the app's buffer as image [b] of the direct present; 0 on success. */
static int dp_import(struct swapchain *sc, uint32_t b)
{
    struct dev *d = sc->d;
    struct direct *dir = sc->dir;
    VkAndroidHardwareBufferFormatPropertiesANDROID fmt = {
        .sType = VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_FORMAT_PROPERTIES_ANDROID };
    VkAndroidHardwareBufferPropertiesANDROID props = {
        .sType = VK_STRUCTURE_TYPE_ANDROID_HARDWARE_BUFFER_PROPERTIES_ANDROID, .pNext = &fmt };
    VkResult r = d->GetAndroidHardwareBufferPropertiesANDROID(d->h, dir->ahb[b], &props);
    if (r != VK_SUCCESS) {
        LOGE("direct present: buffer properties: %d", r);
        return -1;
    }
    if (fmt.format != VK_FORMAT_R8G8B8A8_UNORM) {
        LOGE("direct present: the app's buffer has format %d", fmt.format);
        return -1;
    }
    VkExternalMemoryImageCreateInfo ext = { .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
        .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_ANDROID_HARDWARE_BUFFER_BIT_ANDROID };
    VkImageCreateInfo ici = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        .pNext = &ext,
        .imageType = VK_IMAGE_TYPE_2D,
        .format = VK_FORMAT_R8G8B8A8_UNORM,
        .extent = { sc->extent.width, sc->extent.height, 1 },
        .mipLevels = 1,
        .arrayLayers = 1,
        .samples = VK_SAMPLE_COUNT_1_BIT,
        .tiling = VK_IMAGE_TILING_OPTIMAL,
        .usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
        .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
    };
    r = d->CreateImage(d->h, &ici, NULL, &dir->image[b]);
    if (r != VK_SUCCESS) {
        LOGE("direct present: image: %d", r);
        return -1;
    }
    VkImportAndroidHardwareBufferInfoANDROID imp = {
        .sType = VK_STRUCTURE_TYPE_IMPORT_ANDROID_HARDWARE_BUFFER_INFO_ANDROID, .buffer = dir->ahb[b] };
    VkMemoryDedicatedAllocateInfo ded = { .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
                                          .pNext = &imp, .image = dir->image[b] };
    if (!props.memoryTypeBits)
        return -1;
    VkMemoryAllocateInfo mai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .pNext = &ded,
                                 .allocationSize = props.allocationSize,
                                 .memoryTypeIndex = (uint32_t)__builtin_ctz(props.memoryTypeBits) };
    r = d->AllocateMemory(d->h, &mai, NULL, &dir->mem[b]);
    if (r != VK_SUCCESS) {
        LOGE("direct present: import: %d", r);
        return -1;
    }
    r = d->BindImageMemory(d->h, dir->image[b], dir->mem[b], 0);
    if (r != VK_SUCCESS) {
        LOGE("direct present: bind: %d", r);
        return -1;
    }
    return 0;
}

/* The blit of swapchain image [im] into the app's buffer [b]: the swapchain's B8G8R8A8 lands in the
 * R8G8B8A8 buffer channel for channel; the buffer then goes to the compositor's queue family. */
static VkResult dp_record(struct swapchain *sc, struct img *im, uint32_t b)
{
    struct dev *d = sc->d;
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = sc->pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                        .commandBufferCount = 1 };
    VkResult r = d->AllocateCommandBuffers(d->h, &cai, &im->dcmd[b]);
    if (r != VK_SUCCESS)
        return r;
    VkCommandBuffer cmd = im->dcmd[b];
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    r = d->BeginCommandBuffer(cmd, &bi);
    if (r != VK_SUCCESS)
        return r;
    VkImageMemoryBarrier pre[2] = {
        {
            .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
            .srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT | VK_ACCESS_TRANSFER_WRITE_BIT,
            .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
            .oldLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
            .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
            .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .image = im->image,
            .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
        },
        {
            .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
            .srcAccessMask = 0,
            .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
            .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
            .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
            .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
            .image = sc->dir->image[b],
            .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
        },
    };
    d->CmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL,
                          2, pre);
    const int32_t w = (int32_t)sc->extent.width, h = (int32_t)sc->extent.height;
    VkImageBlit region = {
        .srcSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 },
        .srcOffsets = { { 0, 0, 0 }, { w, h, 1 } },
        .dstSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 },
        .dstOffsets = { { 0, 0, 0 }, { w, h, 1 } },
    };
    d->CmdBlitImage(cmd, im->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, sc->dir->image[b],
                    VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region, VK_FILTER_NEAREST);
    VkImageMemoryBarrier post[2] = { pre[0], pre[1] };
    post[0].srcAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    post[0].dstAccessMask = 0;
    post[0].oldLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    post[0].newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    post[1].srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    post[1].dstAccessMask = 0;
    post[1].oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    post[1].newLayout = VK_IMAGE_LAYOUT_GENERAL;
    if (d->foreign) {
        /* Released to the compositor (VK_EXT_queue_family_foreign): the driver makes the pixels
         * whole for a reader outside Vulkan. */
        post[1].srcQueueFamilyIndex = d->qfam;
        post[1].dstQueueFamilyIndex = VK_QUEUE_FAMILY_FOREIGN_EXT;
    }
    d->CmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 0,
                          NULL, 2, post);
    return d->EndCommandBuffer(cmd);
}

/* Names the swapchain's window in the root window's _PD2_PRESENT_WINDOW property for x-input-relay
 * (the app shows its buffers only while that window is alone on the X screen), as Mesa patch 0004. */
static void dp_name_window(struct swapchain *sc)
{
    static const char name[] = "_PD2_PRESENT_WINDOW";
    xcb_connection_t *c = sc->x.c;
    xcb_intern_atom_cookie_t ac = xcb_intern_atom(c, 0, sizeof name - 1, name);
    xcb_get_geometry_cookie_t gc = xcb_get_geometry(c, sc->x.w);
    xcb_intern_atom_reply_t *atom = xcb_intern_atom_reply(c, ac, NULL);
    xcb_get_geometry_reply_t *geom = xcb_get_geometry_reply(c, gc, NULL);
    if (atom && geom) {
        xcb_window_t w = sc->x.w;
        /* Checked: an error stays here, never reaching the game's own error handler. */
        free(xcb_request_check(c, xcb_change_property_checked(c, XCB_PROP_MODE_REPLACE, geom->root, atom->atom,
                                                              XCB_ATOM_WINDOW, 32, 1, &w)));
    }
    free(atom);
    free(geom);
}

static void dp_close(struct swapchain *sc)
{
    struct direct *dir = sc->dir;
    if (!dir)
        return;
    struct dev *d = sc->d;
    if (dir->sock >= 0)
        shutdown(dir->sock, SHUT_RDWR);
    if (dir->reader_started)
        pthread_join(dir->reader, NULL);
    for (uint32_t b = 0; b < DP_MAX; b++) {
        if (dir->image[b])
            d->DestroyImage(d->h, dir->image[b], NULL);
        if (dir->mem[b])
            d->FreeMemory(d->h, dir->mem[b], NULL);
        if (dir->ahb[b] && g_ahb_release)
            g_ahb_release(dir->ahb[b]);
    }
    if (dir->sock >= 0)
        close(dir->sock);
    free(dir);
    sc->dir = NULL;
}

/* Sets up the direct present of a new swapchain: PD2_PRESENT_SOCKET names the app's socket. On any
 * failure the swapchain's frames go to the X screen. */
static void dp_open(struct swapchain *sc)
{
    const char *path = getenv("PD2_PRESENT_SOCKET");
    if (!path || !*path || !sc->d->ahb || getenv("PD2_HYBRIS_NO_DIRECT"))
        return;
    pthread_once(&g_ahb_once, ahb_load);
    if (!g_ahb_recv || !g_ahb_release)
        return;
    struct direct *dir = calloc(1, sizeof *dir);
    dir->sock = -1;
    /* One more buffer than images: the app keeps the newest frame and the one on the screen, and
     * the game still has one to draw into (the app's server takes up to 5). */
    dir->count = sc->count + 1 < DP_MAX ? sc->count + 1 : DP_MAX;
    sc->dir = dir;
    const char *why = NULL;
    struct dp_msg m;
    int fds[8], nfds;
    dir->sock = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    snprintf(addr.sun_path, sizeof addr.sun_path, "%s", path);
    /* The app answers at once or not at all: a second for the setup. */
    struct timeval timeout = { .tv_sec = 1 };
    if (dir->sock >= 0)
        setsockopt(dir->sock, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
    if (dir->sock < 0 || connect(dir->sock, (struct sockaddr *)&addr, sizeof addr) != 0) {
        why = "no app on the socket";
        goto fail;
    }
    setsockopt(dir->sock, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
    if (!dp_send(dir->sock, DP_HELLO, 0, sc->extent.width, sc->extent.height, dir->count)) {
        why = "HELLO";
        goto fail;
    }
    ssize_t got = dp_recv(dir->sock, &m, fds, 8, &nfds);
    for (int f = 0; f < nfds; f++)
        close(fds[f]);
    if (got != (ssize_t)sizeof m || m.magic != DP_MAGIC || m.version != DP_VERSION || m.type != DP_BUFFERS ||
        m.count != dir->count || m.width != sc->extent.width || m.height != sc->extent.height ||
        m.format != DP_AHB_R8G8B8A8) {
        why = "BUFFERS";
        goto fail;
    }
    for (uint32_t b = 0; b < dir->count; b++) {
        if (g_ahb_recv(dir->sock, &dir->ahb[b]) != 0 || !dir->ahb[b]) {
            dir->ahb[b] = NULL;
            why = "a buffer";
            goto fail;
        }
    }
    timeout.tv_sec = 0;
    setsockopt(dir->sock, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
    for (uint32_t b = 0; b < dir->count; b++) {
        if (dp_import(sc, b) != 0) {
            why = "the phone's driver refused a buffer";
            goto fail;
        }
    }
    for (uint32_t i = 0; i < sc->count; i++) {
        for (uint32_t b = 0; b < dir->count; b++) {
            if (dp_record(sc, &sc->img[i], b) != VK_SUCCESS) {
                why = "command buffers";
                goto fail;
            }
        }
    }
    if (pthread_create(&dir->reader, NULL, dp_reader, sc) != 0) {
        why = "no thread";
        goto fail;
    }
    dir->reader_started = 1;
    dp_name_window(sc);
    LOGE("direct present: %ux%u, %u images into %u app buffers", sc->extent.width, sc->extent.height, sc->count,
         dir->count);
    return;
fail:
    LOGE("direct present off (%s): the X screen", why);
    for (uint32_t i = 0; i < sc->count; i++)
        for (uint32_t b = 0; b < DP_MAX; b++)
            sc->img[i].dcmd[b] = VK_NULL_HANDLE; /* freed with the pool */
    dp_close(sc);
}

/* The app's buffer this present goes to (reserved), waiting for one at most DP_WAIT_NS; -1: the
 * X screen (no direct present, the app has gone or wants the X screen now, or no buffer came
 * back in time). */
static int dp_choose(struct swapchain *sc)
{
    struct direct *dir = sc->dir;
    if (!dir || !pc_direct_wanted())
        return -1;
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_nsec += DP_WAIT_NS;
    deadline.tv_sec += deadline.tv_nsec / 1000000000L;
    deadline.tv_nsec %= 1000000000L;
    int chosen = -1;
    pthread_mutex_lock(&sc->lock);
    while (!dir->gone && chosen < 0) {
        for (uint32_t b = 0; b < dir->count && chosen < 0; b++)
            if (dir->state[b] == DP_FREE)
                chosen = (int)b;
        if (chosen >= 0 || pthread_cond_timedwait(&sc->cond, &sc->lock, &deadline) != 0)
            break;
    }
    if (chosen >= 0)
        dir->state[chosen] = DP_RESERVED;
    pthread_mutex_unlock(&sc->lock);
    return chosen;
}

/* On the present thread, the blit done: the app shows buffer [b]. */
static void dp_present(struct swapchain *sc, int b, VkPresentModeKHR mode)
{
    struct direct *dir = sc->dir;
    pthread_mutex_lock(&sc->lock);
    dir->state[b] = DP_HELD;
    int gone = dir->gone;
    pthread_mutex_unlock(&sc->lock);
    if (gone || !dp_send(dir->sock, DP_PRESENT, (uint32_t)b, sc->extent.width, sc->extent.height, (uint32_t)mode + 1)) {
        /* The app has gone or cannot be told: the next presents go to the X screen. */
        pthread_mutex_lock(&sc->lock);
        dir->gone = 1;
        dir->state[b] = DP_FREE;
        pthread_cond_broadcast(&sc->cond);
        pthread_mutex_unlock(&sc->lock);
        shutdown(dir->sock, SHUT_RDWR);
    }
}

static int mem_type(struct dev *d, uint32_t bits, VkMemoryPropertyFlags want, VkMemoryPropertyFlags need)
{
    for (int pass = 0; pass < 2; pass++)
        for (uint32_t i = 0; i < d->mem.memoryTypeCount; i++) {
            if (!(bits & (1u << i)))
                continue;
            VkMemoryPropertyFlags f = d->mem.memoryTypes[i].propertyFlags;
            if ((f & need) != need)
                continue;
            if (pass == 0 && (f & want) != want)
                continue;
            return (int)i;
        }
    return -1;
}

static void swapchain_free(struct swapchain *sc)
{
    struct dev *d = sc->d;
    VkDevice h = d->h;
    if (sc->thread_started) {
        /* The presents still queued go out first. */
        pthread_mutex_lock(&sc->lock);
        sc->stop = 1;
        pthread_cond_broadcast(&sc->cond);
        pthread_mutex_unlock(&sc->lock);
        pthread_join(sc->thread, NULL);
    }
    dp_close(sc);
    for (uint32_t i = 0; i < sc->count; i++) {
        struct img *im = &sc->img[i];
        if (im->fence)
            d->DestroyFence(h, im->fence, NULL);
        if (im->map)
            d->UnmapMemory(h, im->bufmem);
        if (im->buf)
            d->DestroyBuffer(h, im->buf, NULL);
        if (im->bufmem)
            d->FreeMemory(h, im->bufmem, NULL);
        if (im->image)
            d->DestroyImage(h, im->image, NULL);
        if (im->mem)
            d->FreeMemory(h, im->mem, NULL);
    }
    if (sc->pool)
        d->DestroyCommandPool(h, sc->pool, NULL);
    if (sc->gc)
        xcb_free_gc(sc->x.c, sc->gc);
    free(sc->swz);
    pthread_cond_destroy(&sc->cond);
    pthread_mutex_destroy(&sc->lock);
    free(sc);
}

static VkResult record_copy(struct swapchain *sc, struct img *im)
{
    struct dev *d = sc->d;
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    VkResult r = d->BeginCommandBuffer(im->cmd, &bi);
    if (r != VK_SUCCESS)
        return r;
    VkImageMemoryBarrier b1 = {
        .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT | VK_ACCESS_TRANSFER_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
        .oldLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
        .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .image = im->image,
        .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
    };
    d->CmdPipelineBarrier(im->cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0,
                          NULL, 1, &b1);
    VkBufferImageCopy reg = {
        .bufferOffset = 0,
        .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 },
        .imageExtent = { sc->extent.width, sc->extent.height, 1 },
    };
    d->CmdCopyImageToBuffer(im->cmd, im->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, im->buf, 1, &reg);
    VkImageMemoryBarrier b2 = b1;
    b2.srcAccessMask = 0;
    b2.dstAccessMask = 0;
    b2.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    b2.newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    VkBufferMemoryBarrier bb = {
        .sType = VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_HOST_READ_BIT,
        .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
        .buffer = im->buf,
        .size = VK_WHOLE_SIZE,
    };
    d->CmdPipelineBarrier(im->cmd, VK_PIPELINE_STAGE_TRANSFER_BIT,
                          VK_PIPELINE_STAGE_HOST_BIT | VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 1, &bb, 1,
                          &b2);
    return d->EndCommandBuffer(im->cmd);
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_CreateSwapchainKHR(VkDevice h, const VkSwapchainCreateInfoKHR *ci,
                                                           const VkAllocationCallbacks *a, VkSwapchainKHR *out)
{
    (void)a;
    struct dev *d = dev_get(h);
    if (!d)
        return VK_ERROR_INITIALIZATION_FAILED;
    struct swapchain *sc = calloc(1, sizeof *sc);
    sc->d = d;
    pthread_mutex_init(&sc->lock, NULL);
    {
        pthread_condattr_t ca;
        pthread_condattr_init(&ca);
        pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
        pthread_cond_init(&sc->cond, &ca);
        pthread_condattr_destroy(&ca);
    }
    if (surface_xwin(ci->surface, &sc->x) != 0) {
        free(sc);
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    uint32_t ww, wh;
    if (window_geometry(&sc->x, &ww, &wh, &sc->depth) != 0) {
        free(sc);
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    sc->extent = ci->imageExtent;
    sc->mode = ci->presentMode;
    sc->format = ci->imageFormat;
    sc->swizzle = ci->imageFormat == VK_FORMAT_R8G8B8A8_UNORM || ci->imageFormat == VK_FORMAT_R8G8B8A8_SRGB;
    sc->count = ci->minImageCount < 2 ? 2 : ci->minImageCount;
    if (sc->count > MAX_IMAGES)
        sc->count = MAX_IMAGES;
    sc->bufsize = (VkDeviceSize)sc->extent.width * sc->extent.height * 4;
    if (sc->swizzle)
        sc->swz = malloc(sc->extent.width * 4);
    sc->gc = xcb_generate_id(sc->x.c);
    xcb_create_gc(sc->x.c, sc->gc, sc->x.w, 0, NULL);
    LOGD("swapchain %ux%u format %d, %u images, window depth %u, present mode %d", sc->extent.width,
         sc->extent.height, sc->format, sc->count, sc->depth, ci->presentMode);

    VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                    .queueFamilyIndex = d->qfam };
    VkResult r = d->CreateCommandPool(h, &pci, NULL, &sc->pool);
    for (uint32_t i = 0; r == VK_SUCCESS && i < sc->count; i++) {
        struct img *im = &sc->img[i];
        VkImageCreateInfo ici = {
            .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
            .flags = 0,
            .imageType = VK_IMAGE_TYPE_2D,
            .format = ci->imageFormat,
            .extent = { sc->extent.width, sc->extent.height, 1 },
            .mipLevels = 1,
            .arrayLayers = 1,
            .samples = VK_SAMPLE_COUNT_1_BIT,
            .tiling = VK_IMAGE_TILING_OPTIMAL,
            .usage = ci->imageUsage | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
            .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
            .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
        };
        r = d->CreateImage(h, &ici, NULL, &im->image);
        if (r != VK_SUCCESS)
            break;
        VkMemoryRequirements mr;
        d->GetImageMemoryRequirements(h, im->image, &mr);
        int t = mem_type(d, mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, 0);
        VkMemoryAllocateInfo mai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mr.size,
                                     .memoryTypeIndex = (uint32_t)t };
        r = t < 0 ? VK_ERROR_OUT_OF_DEVICE_MEMORY : d->AllocateMemory(h, &mai, NULL, &im->mem);
        if (r != VK_SUCCESS)
            break;
        r = d->BindImageMemory(h, im->image, im->mem, 0);
        if (r != VK_SUCCESS)
            break;
        VkBufferCreateInfo bci = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sc->bufsize,
                                   .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                                   .sharingMode = VK_SHARING_MODE_EXCLUSIVE };
        r = d->CreateBuffer(h, &bci, NULL, &im->buf);
        if (r != VK_SUCCESS)
            break;
        d->GetBufferMemoryRequirements(h, im->buf, &mr);
        t = mem_type(d, mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_CACHED_BIT, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT);
        if (t < 0) {
            r = VK_ERROR_OUT_OF_HOST_MEMORY;
            break;
        }
        sc->coherent = !!(d->mem.memoryTypes[t].propertyFlags & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
        if (i == 0)
            LOGD("readback memory type %d flags 0x%x", t, d->mem.memoryTypes[t].propertyFlags);
        mai.allocationSize = mr.size;
        mai.memoryTypeIndex = (uint32_t)t;
        r = d->AllocateMemory(h, &mai, NULL, &im->bufmem);
        if (r != VK_SUCCESS)
            break;
        r = d->BindBufferMemory(h, im->buf, im->bufmem, 0);
        if (r != VK_SUCCESS)
            break;
        r = d->MapMemory(h, im->bufmem, 0, VK_WHOLE_SIZE, 0, (void **)&im->map);
        if (r != VK_SUCCESS)
            break;
        VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                            .commandPool = sc->pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                            .commandBufferCount = 1 };
        /* Internal command buffers never reach the loader: their first word stays the driver's
         * own (the HAL's magic), which only a loader would replace. */
        r = d->AllocateCommandBuffers(h, &cai, &im->cmd);
        if (r != VK_SUCCESS)
            break;
        r = record_copy(sc, im);
        if (r != VK_SUCCESS)
            break;
        VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
        r = d->CreateFence(h, &fci, NULL, &im->fence);
    }
    if (r == VK_SUCCESS && pthread_create(&sc->thread, NULL, present_thread, sc) != 0)
        r = VK_ERROR_OUT_OF_HOST_MEMORY;
    if (r != VK_SUCCESS) {
        LOGE("CreateSwapchainKHR failed: %d", r);
        swapchain_free(sc);
        return r;
    }
    sc->thread_started = 1;
    if (ci->oldSwapchain) {
        /* Images of the old swapchain stay valid until it is destroyed; nothing to hand over. */
    }
    dp_open(sc);
    fault_watch_refresh();
    *out = (VkSwapchainKHR)(uintptr_t)sc;
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL pd2_DestroySwapchainKHR(VkDevice h, VkSwapchainKHR s, const VkAllocationCallbacks *a)
{
    (void)h;
    (void)a;
    struct swapchain *sc = (struct swapchain *)(uintptr_t)s;
    if (!sc)
        return;
    LOGD("swapchain destroyed after %" PRIu64 " presents", sc->presents);
    swapchain_free(sc);
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetSwapchainImagesKHR(VkDevice h, VkSwapchainKHR s, uint32_t *count,
                                                              VkImage *images)
{
    (void)h;
    struct swapchain *sc = (struct swapchain *)(uintptr_t)s;
    if (!images) {
        *count = sc->count;
        return VK_SUCCESS;
    }
    uint32_t c = *count < sc->count ? *count : sc->count;
    for (uint32_t i = 0; i < c; i++)
        images[i] = sc->img[i].image;
    *count = c;
    return c < sc->count ? VK_INCOMPLETE : VK_SUCCESS;
}

static VkResult signal_acquire(struct dev *d, VkSemaphore sem, VkFence fence)
{
    if (!sem && !fence)
        return VK_SUCCESS;
    VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO };
    if (sem) {
        si.signalSemaphoreCount = 1;
        si.pSignalSemaphores = &sem;
    }
    pthread_mutex_lock(&d->qlock);
    VkResult r = d->QueueSubmit(d->queue, 1, &si, fence);
    pthread_mutex_unlock(&d->qlock);
    return r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_AcquireNextImageKHR(VkDevice h, VkSwapchainKHR s, uint64_t timeout,
                                                            VkSemaphore sem, VkFence fence, uint32_t *index)
{
    (void)h;
    struct swapchain *sc = (struct swapchain *)(uintptr_t)s;
    uint32_t w, hh;
    if (window_geometry(&sc->x, &w, &hh, NULL) != 0)
        return VK_ERROR_SURFACE_LOST_KHR;
    if (w != sc->extent.width || hh != sc->extent.height)
        return VK_ERROR_OUT_OF_DATE_KHR;
    /* An image whose present is done; the present thread frees them in order. */
    VkResult r = acquire_image(sc, timeout, index);
    return r == VK_SUCCESS ? signal_acquire(sc->d, sem, fence) : r;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_AcquireNextImage2KHR(VkDevice h, const VkAcquireNextImageInfoKHR *ai,
                                                             uint32_t *index)
{
    return pd2_AcquireNextImageKHR(h, ai->swapchain, ai->timeout, ai->semaphore, ai->fence, index);
}

/* On the present thread: the frame onto the X window. */
static void put_image(struct swapchain *sc, struct img *im)
{
    xcb_connection_t *c = sc->x.c;
    uint32_t stride = sc->extent.width * 4;
    /* The Game view's copy, as Mesa's software WSI waits for it (present_counter.h). */
    pc_present_paced();
    /* xcb_get_maximum_request_length is in 4-byte units and counts BIG-REQUESTS. */
    uint32_t max_bytes = xcb_get_maximum_request_length(c) * 4;
    uint32_t rows = max_bytes > 64 ? (max_bytes - 64) / stride : 1;
    if (rows == 0)
        rows = 1;
    for (uint32_t y = 0; y < sc->extent.height; y += rows) {
        uint32_t n = sc->extent.height - y < rows ? sc->extent.height - y : rows;
        const uint8_t *src = im->map + (size_t)y * stride;
        if (sc->swizzle) {
            /* RGBA -> BGRX, one strip at a time (rare: BGRA is offered first). */
            uint8_t *tmp = malloc((size_t)n * stride);
            for (uint32_t k = 0; k < n * sc->extent.width; k++) {
                tmp[k * 4 + 0] = src[k * 4 + 2];
                tmp[k * 4 + 1] = src[k * 4 + 1];
                tmp[k * 4 + 2] = src[k * 4 + 0];
                tmp[k * 4 + 3] = src[k * 4 + 3];
            }
            xcb_put_image(c, XCB_IMAGE_FORMAT_Z_PIXMAP, sc->x.w, sc->gc, sc->extent.width, n, 0, (int16_t)y, 0,
                          sc->depth, n * stride, tmp);
            free(tmp);
        } else {
            xcb_put_image(c, XCB_IMAGE_FORMAT_Z_PIXMAP, sc->x.w, sc->gc, sc->extent.width, n, 0, (int16_t)y, 0,
                          sc->depth, n * stride, src);
        }
    }
    /* A round trip: the X server has the frame when it is counted (on this thread only). */
    free(xcb_get_input_focus_reply(c, xcb_get_input_focus(c), NULL));
}

#include "present_queue.h"

static VKAPI_ATTR VkResult VKAPI_CALL pd2_QueuePresentKHR(VkQueue q, const VkPresentInfoKHR *pi)
{
    struct dev *d = dev_of_queue(q);
    VkResult ret = VK_SUCCESS;
    VkPipelineStageFlags *stages = NULL;
    if (pi->waitSemaphoreCount) {
        stages = malloc(pi->waitSemaphoreCount * sizeof *stages);
        for (uint32_t i = 0; i < pi->waitSemaphoreCount; i++)
            stages[i] = VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
    }
    for (uint32_t i = 0; i < pi->swapchainCount; i++) {
        struct swapchain *sc = (struct swapchain *)(uintptr_t)pi->pSwapchains[i];
        uint32_t index = pi->pImageIndices[i];
        struct img *im = &sc->img[index];
        /* The app's buffer (the direct present), or the copy for the X screen. */
        int b = dp_choose(sc);
        VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1,
                            .pCommandBuffers = b >= 0 ? &im->dcmd[b] : &im->cmd };
        if (i == 0 && pi->waitSemaphoreCount) {
            si.waitSemaphoreCount = pi->waitSemaphoreCount;
            si.pWaitSemaphores = pi->pWaitSemaphores;
            si.pWaitDstStageMask = stages;
        }
        pthread_mutex_lock(&d->qlock);
        VkResult r = d->QueueSubmit(q, 1, &si, im->fence);
        pthread_mutex_unlock(&d->qlock);
        /* The rest is the present thread's; the game's thread goes on. */
        pthread_mutex_lock(&sc->lock);
        if (r == VK_SUCCESS && sc->qn < MAX_IMAGES) {
            present_queued(sc, (struct job){ index, b, sc->mode });
        } else {
            if (r == VK_SUCCESS) {
                /* Never: an image is queued at most once. */
                LOGE("present queue full");
                r = VK_ERROR_OUT_OF_HOST_MEMORY;
            }
            im->state = IMG_FREE;
            if (b >= 0)
                sc->dir->state[b] = DP_FREE;
        }
        pthread_cond_broadcast(&sc->cond);
        pthread_mutex_unlock(&sc->lock);
        if (pi->pResults)
            pi->pResults[i] = r;
        if (r != VK_SUCCESS)
            ret = r;
    }
    free(stages);
    return ret;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetDeviceGroupPresentCapabilitiesKHR(VkDevice h,
                                                                             VkDeviceGroupPresentCapabilitiesKHR *c)
{
    (void)h;
    memset(c->presentMask, 0, sizeof c->presentMask);
    c->presentMask[0] = 1;
    c->modes = VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;
    return VK_SUCCESS;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_GetDeviceGroupSurfacePresentModesKHR(VkDevice h, VkSurfaceKHR s,
                                                                             VkDeviceGroupPresentModeFlagsKHR *m)
{
    (void)h;
    (void)s;
    *m = VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;
    return VK_SUCCESS;
}

/* ------------------------------------------------------------------ desk only */

/* PD2_HYBRIS_DESK_GPU=1 (glibc backend): report lavapipe as an integrated GPU, so that Zink takes
 * its hardware path (kopper on this ICD's swapchain) and the desk test reaches the WSI code. */
static int desk_gpu(void)
{
    static int v = -1;
    if (v < 0) {
        const char *e = getenv("PD2_HYBRIS_DESK_GPU");
        v = g_be.kind == 2 && e && strcmp(e, "1") == 0;
    }
    return v;
}

static VKAPI_ATTR void VKAPI_CALL pd2_GetPhysicalDeviceProperties(VkPhysicalDevice p, VkPhysicalDeviceProperties *pr)
{
    struct inst *in = inst_of(p);
    PFN_vkGetPhysicalDeviceProperties f =
        (PFN_vkGetPhysicalDeviceProperties)g_be.gipa(in->h, "vkGetPhysicalDeviceProperties");
    f(p, pr);
    if (desk_gpu())
        pr->deviceType = VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU;
}

static VKAPI_ATTR void VKAPI_CALL pd2_GetPhysicalDeviceProperties2(VkPhysicalDevice p, VkPhysicalDeviceProperties2 *pr)
{
    struct inst *in = inst_of(p);
    PFN_vkGetPhysicalDeviceProperties2 f =
        (PFN_vkGetPhysicalDeviceProperties2)g_be.gipa(in->h, "vkGetPhysicalDeviceProperties2");
    if (!f)
        f = (PFN_vkGetPhysicalDeviceProperties2)g_be.gipa(in->h, "vkGetPhysicalDeviceProperties2KHR");
    f(p, pr);
    if (desk_gpu())
        pr->properties.deviceType = VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU;
}

/* ------------------------------------------------------------------ pipelines */

/* The SPIR-V the phone's driver gets has no debug names (OpName, OpMemberName). Imagination's
 * C-Series driver 25.3 (Pixel 11, Fact: glrepro's D2GL vertex layouts on a leased cubs/37,
 * 2026-10-28) links a vertex shader's outputs to the fragment shader's inputs by these names, not
 * by Location: Mesa's varying packing leaves two outputs named "v_TexCoord" (Locations 1 and 2) and
 * one input of that name (Location 2), so the fragment shader read the wrong output. A later start
 * hides the bug only because Mesa's disk cache keeps no names (the SPIR-V is the same word for word
 * but for the names): the menu of PD2's first start after setup drew as block noise. Names are for
 * debuggers only; PD2_HYBRIS_KEEP_NAMES=1 keeps them (tests). Returns a copy without them, or NULL
 * (keep the code as it is: kept names, not SPIR-V, or no memory). */
static uint32_t *spirv_without_names(const uint32_t *code, size_t size, size_t *out_size)
{
    static int keep = -1;
    if (keep < 0) {
        const char *e = getenv("PD2_HYBRIS_KEEP_NAMES");
        keep = (e && strcmp(e, "1") == 0) || g_be.kind != 1;
    }
    if (keep || !code || size < 20 || size % 4 || code[0] != 0x07230203u)
        return NULL;
    size_t n = size / 4, o = 5;
    uint32_t *out = malloc(size);
    if (!out)
        return NULL;
    memcpy(out, code, 5 * 4);
    for (size_t i = 5; i < n;) {
        uint32_t wc = code[i] >> 16, op = code[i] & 0xffffu;
        if (wc == 0 || i + wc > n) {
            free(out);
            return NULL;
        }
        if (op != 5 && op != 6) { /* OpName, OpMemberName */
            memcpy(out + o, code + i, wc * 4);
            o += wc;
        }
        i += wc;
    }
    *out_size = o * 4;
    return out;
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_CreateShaderModule(VkDevice h, const VkShaderModuleCreateInfo *ci,
                                                           const VkAllocationCallbacks *a, VkShaderModule *out)
{
    struct dev *d = dev_get(h);
    size_t size = 0;
    uint32_t *code = spirv_without_names(ci->pCode, ci->codeSize, &size);
    if (!code)
        return d->CreateShaderModule(h, ci, a, out);
    VkShaderModuleCreateInfo c = *ci;
    c.pCode = code;
    c.codeSize = size;
    VkResult r = d->CreateShaderModule(h, &c, a, out);
    free(code);
    return r;
}

/* The same for a pipeline's stages whose code comes in a VkShaderModuleCreateInfo first in their
 * pNext (VK_KHR_maintenance5 or a library); [copies] gets the stage and code copies to free. */
static const VkGraphicsPipelineCreateInfo *stages_without_names(const VkGraphicsPipelineCreateInfo *ci,
                                                                VkGraphicsPipelineCreateInfo *pci, void **copies)
{
    *copies = NULL;
    int any = 0;
    for (uint32_t s = 0; s < ci->stageCount; s++) {
        const VkBaseInStructure *p = ci->pStages[s].pNext;
        any |= p && p->sType == VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO;
    }
    if (!any)
        return ci;
    /* One block: the stages, their module infos, then the codes' pointers. */
    size_t block = ci->stageCount * (sizeof(VkPipelineShaderStageCreateInfo) + sizeof(VkShaderModuleCreateInfo) +
                                     sizeof(uint32_t *));
    char *mem = calloc(1, block);
    if (!mem)
        return ci;
    VkPipelineShaderStageCreateInfo *st = (VkPipelineShaderStageCreateInfo *)mem;
    VkShaderModuleCreateInfo *mi = (VkShaderModuleCreateInfo *)(st + ci->stageCount);
    uint32_t **codes = (uint32_t **)(mi + ci->stageCount);
    for (uint32_t s = 0; s < ci->stageCount; s++) {
        st[s] = ci->pStages[s];
        const VkBaseInStructure *p = st[s].pNext;
        if (!p || p->sType != VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO)
            continue;
        const VkShaderModuleCreateInfo *m = (const VkShaderModuleCreateInfo *)p;
        size_t size = 0;
        codes[s] = spirv_without_names(m->pCode, m->codeSize, &size);
        if (!codes[s])
            continue;
        mi[s] = *m;
        mi[s].pCode = codes[s];
        mi[s].codeSize = size;
        st[s].pNext = &mi[s];
    }
    *pci = *ci;
    pci->pStages = st;
    *copies = mem;
    return pci;
}

static void free_stage_copies(void *mem, uint32_t stages)
{
    if (!mem)
        return;
    VkPipelineShaderStageCreateInfo *st = mem;
    VkShaderModuleCreateInfo *mi = (VkShaderModuleCreateInfo *)(st + stages);
    uint32_t **codes = (uint32_t **)(mi + stages);
    for (uint32_t s = 0; s < stages; s++)
        free(codes[s]);
    free(mem);
}

static VKAPI_ATTR VkResult VKAPI_CALL pd2_CreateGraphicsPipelines(VkDevice h, VkPipelineCache cache, uint32_t n,
                                                                const VkGraphicsPipelineCreateInfo *ci,
                                                                const VkAllocationCallbacks *a, VkPipeline *out)
{
    struct dev *d = dev_get(h);
    int inline_code = 0;
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t s = 0; s < ci[i].stageCount; s++) {
            const VkBaseInStructure *p = ci[i].pStages[s].pNext;
            inline_code |= p && p->sType == VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO;
        }
    if (!inline_code)
        return d->CreateGraphicsPipelines(h, cache, n, ci, a, out);
    /* Each pipeline on its own, with its stages' inline code without names. */
    VkResult r = VK_SUCCESS;
    for (uint32_t i = 0; i < n; i++)
        out[i] = VK_NULL_HANDLE;
    for (uint32_t i = 0; i < n; i++) {
        VkGraphicsPipelineCreateInfo pci;
        void *copies;
        const VkGraphicsPipelineCreateInfo *use = stages_without_names(&ci[i], &pci, &copies);
        VkResult ri = d->CreateGraphicsPipelines(h, cache, 1, use, a, &out[i]);
        free_stage_copies(copies, ci[i].stageCount);
        if (ri != VK_SUCCESS) {
            r = ri;
            if (ci[i].flags & VK_PIPELINE_CREATE_EARLY_RETURN_ON_FAILURE_BIT)
                break;
        }
    }
    return r;
}

/* ------------------------------------------------------------------ dispatch */

struct entry {
    const char *name;
    PFN_vkVoidFunction fn;
};
#define E(n) { "vk" #n, (PFN_vkVoidFunction)pd2_##n }
static const struct entry k_global[] = {
    E(EnumerateInstanceExtensionProperties),
    E(EnumerateInstanceLayerProperties),
    E(EnumerateInstanceVersion),
    E(CreateInstance),
};
static const struct entry k_instance[] = {
    E(DestroyInstance),
    E(EnumeratePhysicalDevices),
    E(EnumeratePhysicalDeviceGroups),
    { "vkEnumeratePhysicalDeviceGroupsKHR", (PFN_vkVoidFunction)pd2_EnumeratePhysicalDeviceGroups },
    E(EnumerateDeviceExtensionProperties),
    E(CreateDevice),
    E(DestroySurfaceKHR),
    E(GetPhysicalDeviceSurfaceSupportKHR),
    E(GetPhysicalDeviceSurfaceCapabilitiesKHR),
    E(GetPhysicalDeviceSurfaceFormatsKHR),
    E(GetPhysicalDeviceSurfacePresentModesKHR),
    E(GetPhysicalDevicePresentRectanglesKHR),
    E(GetPhysicalDeviceXcbPresentationSupportKHR),
    E(GetPhysicalDeviceXlibPresentationSupportKHR),
};
static const struct entry k_device[] = {
    E(DestroyDevice),
    E(GetDeviceQueue),
    E(GetDeviceQueue2),
    E(QueueSubmit),
    E(QueueSubmit2),
    { "vkQueueSubmit2KHR", (PFN_vkVoidFunction)pd2_QueueSubmit2 },
    E(QueueWaitIdle),
    E(QueueBindSparse),
    E(DeviceWaitIdle),
    E(CreateSwapchainKHR),
    E(DestroySwapchainKHR),
    E(GetSwapchainImagesKHR),
    E(AcquireNextImageKHR),
    E(AcquireNextImage2KHR),
    E(QueuePresentKHR),
    E(GetDeviceGroupPresentCapabilitiesKHR),
    E(GetDeviceGroupSurfacePresentModesKHR),
    E(CreateGraphicsPipelines),
    E(CreateShaderModule),
};
#undef E

static PFN_vkVoidFunction lookup(const struct entry *t, size_t n, const char *name)
{
    for (size_t i = 0; i < n; i++)
        if (strcmp(t[i].name, name) == 0)
            return t[i].fn;
    return NULL;
}

static VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL pd2_GetDeviceProcAddr(VkDevice h, const char *name)
{
    if (strcmp(name, "vkGetDeviceProcAddr") == 0)
        return (PFN_vkVoidFunction)pd2_GetDeviceProcAddr;
    PFN_vkVoidFunction f = lookup(k_device, ARRAY_LEN(k_device), name);
    if (f) {
        if (strcmp(name, "vkQueueSubmit2") == 0 || strcmp(name, "vkQueueSubmit2KHR") == 0) {
            struct dev *d = dev_get(h);
            if (!d || !d->QueueSubmit2)
                return NULL;
        }
        return f;
    }
    struct dev *d = dev_get(h);
    if (!d)
        return NULL;
    return d->GetDeviceProcAddr(h, name);
}

static VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL pd2_GetInstanceProcAddr(VkInstance h, const char *name)
{
    if (!name)
        return NULL;
    if (strcmp(name, "vkGetInstanceProcAddr") == 0)
        return (PFN_vkVoidFunction)pd2_GetInstanceProcAddr;
    PFN_vkVoidFunction f = lookup(k_global, ARRAY_LEN(k_global), name);
    if (f)
        return f;
    if (!h)
        return NULL;
    if (strcmp(name, "vkGetDeviceProcAddr") == 0)
        return (PFN_vkVoidFunction)pd2_GetDeviceProcAddr;
    if (backend() && desk_gpu()) {
        if (strcmp(name, "vkGetPhysicalDeviceProperties") == 0)
            return (PFN_vkVoidFunction)pd2_GetPhysicalDeviceProperties;
        if (strcmp(name, "vkGetPhysicalDeviceProperties2") == 0 ||
            strcmp(name, "vkGetPhysicalDeviceProperties2KHR") == 0)
            return (PFN_vkVoidFunction)pd2_GetPhysicalDeviceProperties2;
    }
    f = lookup(k_instance, ARRAY_LEN(k_instance), name);
    if (f)
        return f;
    f = lookup(k_device, ARRAY_LEN(k_device), name);
    if (f)
        return f;
    /* Surface creation stays with the loader (it makes VkIcdSurfaceXlib/Xcb itself). */
    if (strstr(name, "SurfaceKHR") && strncmp(name, "vkCreate", 8) == 0)
        return NULL;
    if (!backend())
        return NULL;
    return g_be.gipa(h, name);
}

/* ------------------------------------------------------------------ loader-ICD interface */

PD2_EXPORT VKAPI_ATTR VkResult VKAPI_CALL vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *v)
{
    if (*v > 5)
        *v = 5;
    LOGD("loader interface version %u", *v);
    return VK_SUCCESS;
}

PD2_EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vk_icdGetInstanceProcAddr(VkInstance h, const char *name)
{
    return pd2_GetInstanceProcAddr(h, name);
}

PD2_EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vk_icdGetPhysicalDeviceProcAddr(VkInstance h, const char *name)
{
    PFN_vkVoidFunction f = lookup(k_instance, ARRAY_LEN(k_instance), name);
    if (f)
        return f;
    if (!backend())
        return NULL;
    return g_be.gipa(h, name);
}
