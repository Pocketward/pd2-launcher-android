// The device check's probe of the phone's own Vulkan driver, for the system-driver mode (Zink ->
// Venus -> virglrenderer's server -> this driver). Runs in
// the app's process (bionic), once per process (PhoneDriver), on every GPU: where Turnip or PanVK
// drives it too, the report decides whether Play offers the phone's own driver. It loads
// Android's libvulkan.so at run time and
// reports, as "key=value" lines, what decides whether the bridge and Zink can run:
//   - the device's Vulkan version and name, the driver's id, name and version number
//     (VkPhysicalDeviceProperties.driverVersion as the driver gives it: Venus replaces it with
//     Mesa's own version, Mesa src/virtio/vulkan/vn_physical_device.c, and that is the value
//     Zink's driver checks read; Imagination's driver gives its DDK change number, which Zink
//     main compares with 6554834, DDK 24.1, in zink_screen.c);
//   - Zink 25.1.9's hard needs (Mesa mesa-25.1.9 src/gallium/drivers/zink/zink_device_info.py
//     required=True and zink_screen.c): maintenance1, create_renderpass2, imageless_framebuffer,
//     descriptor_update_template (each an extension or core in the device's version) and the
//     timelineSemaphore feature; and the geometryShader feature, without which Zink 25.1.9 fails
//     on Imagination's own driver (zink_screen.c:3411-3415; Venus passes the driver's real id to
//     Zink through VK_KHR_maintenance7's layered properties, vn_physical_device.c);
//   - Zink's base requirements (check_base_requirements, zink_screen.c: Zink only warns
//     without them, the app gives no Venus route): logicOp, fillModeNonSolid and
//     shaderClipDistance (the last two in 25.1.9 only), scalarBlockLayout, custom border colours,
//     line rasterization;
//   - Zink 26.2.3's further needs, the same in main at userland/config.sh's MESA_COMMIT (our
//     product Mesa, which the mode uses where the driver meets them): dynamicRendering,
//     maintenance5 and robustness2's nullDescriptor;
//   - how virglrenderer can share host-visible memory (virglrenderer src/venus/
//     vkr_physical_device.c, the external buffer query with TRANSFER_SRC usage): a dma-buf it can
//     export, an opaque fd it can export (and whether only of dedicated allocations, which the
//     server does not count), whether the driver has VK_EXT_external_memory_dma_buf (needed to
//     import a dma-buf, our patch 0002's path), and whether it imports AHardwareBuffers
//     (VK_ANDROID_external_memory_android_hardware_buffer with VK_EXT_queue_family_foreign, our
//     patch 0005's path for a driver with no dma-buf).
// Written from the Vulkan specification (vkGetPhysicalDeviceFeatures, vkGetPhysicalDeviceFeatures2,
// vkGetPhysicalDeviceExternalBufferProperties). No third-party code.
#include <dlfcn.h>
#include <jni.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

#define OUT_MAX 2048

static int has_ext(const VkExtensionProperties *e, uint32_t n, const char *name)
{
    for (uint32_t i = 0; i < n; i++)
        if (!strcmp(e[i].extensionName, name))
            return 1;
    return 0;
}

static void put(char *out, const char *fmt, ...) __attribute__((format(printf, 2, 3)));

static void put(char *out, const char *fmt, ...)
{
    size_t used = strlen(out);
    if (used >= OUT_MAX - 1)
        return;
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(out + used, OUT_MAX - used, fmt, ap);
    va_end(ap);
}

// The external buffer features for one handle type (0 when the query is not there).
static VkExternalMemoryFeatureFlags external_buffer(PFN_vkGetPhysicalDeviceExternalBufferProperties get,
                                                    VkPhysicalDevice pd,
                                                    VkExternalMemoryHandleTypeFlagBits type,
                                                    VkExternalMemoryHandleTypeFlags *export_from)
{
    VkPhysicalDeviceExternalBufferInfo info = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_BUFFER_INFO,
        .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
        .handleType = type,
    };
    VkExternalBufferProperties props = {.sType = VK_STRUCTURE_TYPE_EXTERNAL_BUFFER_PROPERTIES};
    *export_from = 0;
    if (!get)
        return 0;
    get(pd, &info, &props);
    *export_from = props.externalMemoryProperties.exportFromImportedHandleTypes;
    return props.externalMemoryProperties.externalMemoryFeatures;
}

static void probe(char *out)
{
    void *lib = dlopen("libvulkan.so", RTLD_NOW | RTLD_LOCAL);
    if (!lib) {
        put(out, "error=no libvulkan.so\n");
        return;
    }
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    PFN_vkCreateInstance create = gipa ? (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance") : NULL;
    PFN_vkEnumerateInstanceVersion instance_version =
        gipa ? (PFN_vkEnumerateInstanceVersion)gipa(NULL, "vkEnumerateInstanceVersion") : NULL;
    if (!create) {
        put(out, "error=no vkCreateInstance\n");
        dlclose(lib);
        return;
    }
    uint32_t loader = VK_API_VERSION_1_0;
    if (instance_version)
        instance_version(&loader);
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "pd2-system-driver-probe",
        // The loader's version, so that the 1.3 and 1.4 feature structures below may be queried.
        .apiVersion = loader,
    };
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance inst;
    VkResult r = create(&ici, NULL, &inst);
    if (r != VK_SUCCESS) {
        put(out, "error=vkCreateInstance %d\n", r);
        dlclose(lib);
        return;
    }
#define GET(name) PFN_##name name = (PFN_##name)gipa(inst, #name)
    GET(vkDestroyInstance);
    GET(vkEnumeratePhysicalDevices);
    GET(vkGetPhysicalDeviceProperties);
    GET(vkGetPhysicalDeviceProperties2);
    GET(vkGetPhysicalDeviceFeatures);
    GET(vkGetPhysicalDeviceFeatures2);
    GET(vkEnumerateDeviceExtensionProperties);
    GET(vkGetPhysicalDeviceExternalBufferProperties);
    uint32_t n = 1;
    VkPhysicalDevice pd = VK_NULL_HANDLE;
    r = vkEnumeratePhysicalDevices ? vkEnumeratePhysicalDevices(inst, &n, &pd) : VK_ERROR_INITIALIZATION_FAILED;
    if ((r != VK_SUCCESS && r != VK_INCOMPLETE) || n == 0 || pd == VK_NULL_HANDLE) {
        put(out, "error=no physical device (%d)\n", r);
        vkDestroyInstance(inst, NULL);
        dlclose(lib);
        return;
    }
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(pd, &props);
    const uint32_t api = props.apiVersion;
    put(out, "name=%s\napi=%u.%u.%u\ndriver_version=%u\n", props.deviceName, VK_API_VERSION_MAJOR(api),
        VK_API_VERSION_MINOR(api), VK_API_VERSION_PATCH(api), props.driverVersion);

    uint32_t ne = 0;
    vkEnumerateDeviceExtensionProperties(pd, NULL, &ne, NULL);
    VkExtensionProperties *e = calloc(ne ? ne : 1, sizeof(*e));
    if (e)
        vkEnumerateDeviceExtensionProperties(pd, NULL, &ne, e);
    else
        ne = 0;
    const int v11 = api >= VK_API_VERSION_1_1, v12 = api >= VK_API_VERSION_1_2;

    if (vkGetPhysicalDeviceProperties2 && (v12 || has_ext(e, ne, "VK_KHR_driver_properties"))) {
        VkPhysicalDeviceDriverProperties drv = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
        VkPhysicalDeviceProperties2 p2 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &drv};
        vkGetPhysicalDeviceProperties2(pd, &p2);
        put(out, "driver_id=%d\ndriver=%s %s\n", drv.driverID, drv.driverName, drv.driverInfo);
    }

    // Timeline semaphores: the Vulkan 1.2 feature, or VK_KHR_timeline_semaphore's.
    int timeline = 0, imageless = 0;
    if (vkGetPhysicalDeviceFeatures2) {
        VkPhysicalDeviceVulkan12Features f12 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES};
        VkPhysicalDeviceTimelineSemaphoreFeatures tl = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES};
        VkPhysicalDeviceImagelessFramebufferFeatures il = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGELESS_FRAMEBUFFER_FEATURES};
        VkPhysicalDeviceFeatures2 f2 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2};
        if (v12) {
            f2.pNext = &f12;
        } else {
            void **next = &f2.pNext;
            if (has_ext(e, ne, "VK_KHR_timeline_semaphore")) { *next = &tl; next = &tl.pNext; }
            if (has_ext(e, ne, "VK_KHR_imageless_framebuffer")) { *next = &il; next = &il.pNext; }
        }
        vkGetPhysicalDeviceFeatures2(pd, &f2);
        timeline = v12 ? f12.timelineSemaphore : tl.timelineSemaphore;
        imageless = v12 ? f12.imagelessFramebuffer : il.imagelessFramebuffer;
    }
    put(out, "maintenance1=%d\ncreate_renderpass2=%d\ndescriptor_update_template=%d\nimageless_framebuffer=%d\n"
             "timeline_semaphore=%d\n",
        v11 || has_ext(e, ne, "VK_KHR_maintenance1"), v12 || has_ext(e, ne, "VK_KHR_create_renderpass2"),
        v11 || has_ext(e, ne, "VK_KHR_descriptor_update_template"), imageless, timeline);

    // Geometry shaders (a Vulkan 1.0 feature): Zink 25.1.9 fails on Imagination's own driver
    // without them (zink_screen.c:3411-3415). And the Vulkan 1.0 features of Zink's base
    // requirements (check_base_requirements: mesa-25.1.9 zink_screen.c:2722-2729, main at
    // MESA_COMMIT zink_screen.c:2995-3000): logicOp, and in 25.1.9 also fillModeNonSolid and
    // shaderClipDistance.
    // No line without the query: the app then knows nothing about them and applies no rule.
    if (vkGetPhysicalDeviceFeatures) {
        VkPhysicalDeviceFeatures base = {0};
        vkGetPhysicalDeviceFeatures(pd, &base);
        put(out, "geometry_shader=%d\nlogic_op=%d\nfill_mode_non_solid=%d\nshader_clip_distance=%d\n",
            base.geometryShader ? 1 : 0, base.logicOp ? 1 : 0, base.fillModeNonSolid ? 1 : 0, base.shaderClipDistance ? 1 : 0);
    }

    // The rest of Zink's base requirements (maintenance1 is above): scalarBlockLayout (the Vulkan
    // 1.2 feature, or VK_EXT_scalar_block_layout's), VK_EXT_custom_border_color with its
    // customBorderColors feature (zink_device_info.py: the extension counts only with it) and
    // VK_EXT_line_rasterization (Zink reads the EXT name only; Venus lists it only where the
    // driver does).
    int scalar_block_layout = 0, custom_border_color = 0;
    if (vkGetPhysicalDeviceFeatures2) {
        const int sbl_ext = has_ext(e, ne, "VK_EXT_scalar_block_layout");
        const int cbc_ext = has_ext(e, ne, "VK_EXT_custom_border_color");
        VkPhysicalDeviceVulkan12Features f12 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES};
        VkPhysicalDeviceScalarBlockLayoutFeatures sbl = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SCALAR_BLOCK_LAYOUT_FEATURES};
        VkPhysicalDeviceCustomBorderColorFeaturesEXT cbc = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_CUSTOM_BORDER_COLOR_FEATURES_EXT};
        VkPhysicalDeviceFeatures2 f2 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2};
        void **next = &f2.pNext;
        if (v12) { *next = &f12; next = &f12.pNext; }
        else if (sbl_ext) { *next = &sbl; next = &sbl.pNext; }
        if (cbc_ext) { *next = &cbc; next = &cbc.pNext; }
        vkGetPhysicalDeviceFeatures2(pd, &f2);
        scalar_block_layout = v12 ? f12.scalarBlockLayout : sbl_ext && sbl.scalarBlockLayout;
        custom_border_color = cbc_ext && cbc.customBorderColors;
    }
    put(out, "scalar_block_layout=%d\ncustom_border_color=%d\nline_rasterization=%d\n", scalar_block_layout ? 1 : 0,
        custom_border_color ? 1 : 0, has_ext(e, ne, "VK_EXT_line_rasterization"));

    // Zink 26.2.3's further hard needs (mesa-26.2.3 zink_device_info.py required=True and
    // zink_screen.c): dynamicRendering (core in Vulkan 1.3, else VK_KHR_dynamic_rendering),
    // maintenance5 (core in 1.4, else VK_KHR_maintenance5) and robustness2's nullDescriptor
    // (VK_EXT_robustness2 or VK_KHR_robustness2; the same structure).
    int dynamic_rendering = 0, maintenance5 = 0, null_descriptor = 0;
    if (vkGetPhysicalDeviceFeatures2) {
        const int v13 = api >= VK_API_VERSION_1_3, v14 = api >= VK_MAKE_API_VERSION(0, 1, 4, 0);
        const int rb2 = has_ext(e, ne, "VK_EXT_robustness2") || has_ext(e, ne, "VK_KHR_robustness2");
        VkPhysicalDeviceDynamicRenderingFeatures dr = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES};
        VkPhysicalDeviceMaintenance5FeaturesKHR m5 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_5_FEATURES_KHR};
        VkPhysicalDeviceRobustness2FeaturesEXT r2 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ROBUSTNESS_2_FEATURES_EXT};
        VkPhysicalDeviceFeatures2 f2 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2};
        void **next = &f2.pNext;
        if (v13 || has_ext(e, ne, "VK_KHR_dynamic_rendering")) { *next = &dr; next = &dr.pNext; }
        if (v14 || has_ext(e, ne, "VK_KHR_maintenance5")) { *next = &m5; next = &m5.pNext; }
        if (rb2) { *next = &r2; next = &r2.pNext; }
        vkGetPhysicalDeviceFeatures2(pd, &f2);
        dynamic_rendering = dr.dynamicRendering;
        maintenance5 = m5.maintenance5;
        null_descriptor = rb2 && r2.nullDescriptor;
    }
    put(out, "dynamic_rendering=%d\nmaintenance5=%d\nnull_descriptor=%d\n", dynamic_rendering, maintenance5, null_descriptor);

    // How virglrenderer would share host-visible memory.
    VkExternalMemoryHandleTypeFlags from;
    const int dma_buf_ext = has_ext(e, ne, "VK_EXT_external_memory_dma_buf");
    VkExternalMemoryFeatureFlags f = dma_buf_ext
        ? external_buffer(vkGetPhysicalDeviceExternalBufferProperties, pd, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, &from)
        : 0;
    put(out, "dma_buf_extension=%d\ndma_buf_export=%d\n", dma_buf_ext,
        (f & VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT) && (from & VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT) ? 1 : 0);
    // The zero-copy present (Mesa WSI patch 0004 through Venus): the app's buffers imported as
    // dma-bufs into linear images with an explicit DRM format modifier.
    put(out, "dma_buf_import=%d\ndrm_format_modifier=%d\n", (f & VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT) ? 1 : 0,
        has_ext(e, ne, "VK_EXT_image_drm_format_modifier"));
    f = has_ext(e, ne, "VK_KHR_external_memory_fd")
        ? external_buffer(vkGetPhysicalDeviceExternalBufferProperties, pd, VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT, &from)
        : 0;
    put(out, "opaque_fd_export=%d\nopaque_fd_dedicated_only=%d\n",
        (f & VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT) && (from & VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT) ? 1 : 0,
        (f & VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT) ? 1 : 0);
    put(out, "ahb=%d\nqueue_family_foreign=%d\n", has_ext(e, ne, "VK_ANDROID_external_memory_android_hardware_buffer"),
        has_ext(e, ne, "VK_EXT_queue_family_foreign"));
    free(e);
    vkDestroyInstance(inst, NULL);
    dlclose(lib);
}

JNIEXPORT jstring JNICALL Java_com_pd2launcher_app_SystemDriverProbe_nativeProbe(JNIEnv *env, jclass cls)
{
    (void)cls;
    char out[OUT_MAX] = "";
    probe(out);
    // The driver's own strings need not be ASCII; NewStringUTF takes modified UTF-8 only.
    for (char *c = out; *c; c++)
        if ((unsigned char)*c >= 0x80 || ((unsigned char)*c < 0x20 && *c != '\n'))
            *c = '?';
    return (*env)->NewStringUTF(env, out);
}
