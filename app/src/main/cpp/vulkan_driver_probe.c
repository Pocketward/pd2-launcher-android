// The facts of the phone's own Vulkan driver, for the TP UBWC flag hint of the game's Turnip on an
// Adreno 740 (UbwcFlagHint.kt) and for device.txt (every GPU): the device's name, its Vulkan
// version, the driver's version (VkPhysicalDeviceProperties.driverVersion) and, where the device
// has Vulkan 1.2 or VK_KHR_driver_properties, the driver's name, its info text (Qualcomm's carries
// the build hash and date) and the conformance version (VkPhysicalDeviceDriverProperties). It
// loads Android's libvulkan.so at run time, so the phone's driver is in the app's process for the
// time of the query only. The Kotlin side guards the call with a marker file (VulkanDriverProbe).
// Written from the Vulkan specification (vkCreateInstance, vkEnumeratePhysicalDevices,
// vkGetPhysicalDeviceProperties, vkGetPhysicalDeviceProperties2,
// vkEnumerateDeviceExtensionProperties). No third-party code.
#include <dlfcn.h>
#include <jni.h>
#include <stdio.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <vulkan/vulkan.h>

// Appends "<key>=<text>\n" to the buffer. A line break in the text becomes "; " (Qualcomm's
// driverInfo has one line per fact); any other text outside printable ASCII becomes '?' (a line must
// not break, and the Java string must be valid).
static void put_text(char *out, size_t size, size_t *used, const char *key, const char *text) {
    size_t at = *used;
    for (const char *p = key; *p != '\0' && at + 2 < size; p++) out[at++] = *p;
    if (at + 2 < size) out[at++] = '=';
    int line_break = 0;
    for (const char *p = text; *p != '\0' && at + 2 < size; p++) {
        unsigned char c = (unsigned char)*p;
        if (c == '\n' || c == '\r') {
            line_break = 1;
            continue;
        }
        if (line_break && at + 4 < size) {
            out[at++] = ';';
            out[at++] = ' ';
        }
        line_break = 0;
        out[at++] = (c >= 0x20 && c <= 0x7e) ? (char)c : '?';
    }
    out[at++] = '\n';
    out[at] = '\0';
    *used = at;
}

static void put_number(char *out, size_t size, size_t *used, const char *key, unsigned long long value) {
    char text[32];
    snprintf(text, sizeof text, "%llu", value);
    put_text(out, size, used, key, text);
}

// Returns the facts as "key=value" lines (device, api, driver, and where known name, info, conf),
// or null when no Vulkan device answers.
JNIEXPORT jstring JNICALL Java_com_pd2launcher_app_VulkanDriverProbe_nativeDriverFacts(JNIEnv *env, jclass cls) {
    (void)cls;
    char out[2048];
    size_t used = 0;
    out[0] = '\0';
    int found = 0;
    void *lib = dlopen("libvulkan.so", RTLD_NOW | RTLD_LOCAL);
    if (lib == NULL) return NULL;
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    PFN_vkCreateInstance create = gipa != NULL ? (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance") : NULL;
    // Vulkan 1.1 instance first, for vkGetPhysicalDeviceProperties2; a loader that refuses it gets 1.0.
    uint32_t levels[2] = {VK_API_VERSION_1_1, VK_API_VERSION_1_0};
    VkInstance instance = VK_NULL_HANDLE;
    for (int i = 0; create != NULL && i < 2 && instance == VK_NULL_HANDLE; i++) {
        VkApplicationInfo app = {
            .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
            .pApplicationName = "pd2-vulkan-driver-probe",
            .apiVersion = levels[i],
        };
        VkInstanceCreateInfo info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
        if (create(&info, NULL, &instance) != VK_SUCCESS) instance = VK_NULL_HANDLE;
    }
    if (instance != VK_NULL_HANDLE) {
        PFN_vkDestroyInstance destroy = (PFN_vkDestroyInstance)gipa(instance, "vkDestroyInstance");
        PFN_vkEnumeratePhysicalDevices enumerate =
            (PFN_vkEnumeratePhysicalDevices)gipa(instance, "vkEnumeratePhysicalDevices");
        PFN_vkGetPhysicalDeviceProperties properties =
            (PFN_vkGetPhysicalDeviceProperties)gipa(instance, "vkGetPhysicalDeviceProperties");
        PFN_vkGetPhysicalDeviceProperties2 properties2 =
            (PFN_vkGetPhysicalDeviceProperties2)gipa(instance, "vkGetPhysicalDeviceProperties2");
        PFN_vkEnumerateDeviceExtensionProperties extensions =
            (PFN_vkEnumerateDeviceExtensionProperties)gipa(instance, "vkEnumerateDeviceExtensionProperties");
        // One device is enough: VK_INCOMPLETE says that there are more.
        uint32_t count = 1;
        VkPhysicalDevice device = VK_NULL_HANDLE;
        VkResult result = enumerate != NULL ? enumerate(instance, &count, &device) : VK_ERROR_INITIALIZATION_FAILED;
        if ((result == VK_SUCCESS || result == VK_INCOMPLETE) && count > 0 && device != VK_NULL_HANDLE &&
            properties != NULL) {
            VkPhysicalDeviceProperties props;
            properties(device, &props);
            found = 1;
            put_text(out, sizeof out, &used, "device", props.deviceName);
            put_number(out, sizeof out, &used, "api", props.apiVersion);
            put_number(out, sizeof out, &used, "driver", props.driverVersion);
            // The driver's own properties: Vulkan 1.2, or the extension that has them before.
            int has_driver_properties = props.apiVersion >= VK_API_VERSION_1_2;
            if (!has_driver_properties && extensions != NULL) {
                uint32_t n = 0;
                if (extensions(device, NULL, &n, NULL) == VK_SUCCESS && n > 0 && n <= 256) {
                    VkExtensionProperties list[256];
                    if (extensions(device, NULL, &n, list) == VK_SUCCESS) {
                        for (uint32_t i = 0; i < n; i++) {
                            if (strcmp(list[i].extensionName, "VK_KHR_driver_properties") == 0) {
                                has_driver_properties = 1;
                            }
                        }
                    }
                }
            }
            if (has_driver_properties && properties2 != NULL) {
                VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
                VkPhysicalDeviceProperties2 both = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &driver};
                properties2(device, &both);
                driver.driverName[VK_MAX_DRIVER_NAME_SIZE - 1] = '\0';
                driver.driverInfo[VK_MAX_DRIVER_INFO_SIZE - 1] = '\0';
                put_text(out, sizeof out, &used, "name", driver.driverName);
                put_text(out, sizeof out, &used, "info", driver.driverInfo);
                char conformance[32];
                snprintf(conformance, sizeof conformance, "%u.%u.%u.%u", (unsigned)driver.conformanceVersion.major,
                         (unsigned)driver.conformanceVersion.minor, (unsigned)driver.conformanceVersion.subminor,
                         (unsigned)driver.conformanceVersion.patch);
                put_text(out, sizeof out, &used, "conf", conformance);
            }
        }
        if (destroy != NULL) destroy(instance, NULL);
    }
    dlclose(lib);
    return found ? (*env)->NewStringUTF(env, out) : NULL;
}
