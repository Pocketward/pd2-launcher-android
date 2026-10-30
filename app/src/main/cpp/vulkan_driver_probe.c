// The version of the phone's own Vulkan driver (VkPhysicalDeviceProperties.driverVersion), for the
// TP UBWC flag hint of the game's Turnip on an Adreno 740 (UbwcFlagHint.kt). It loads Android's
// libvulkan.so at run time, so the phone's driver is in the app's process for the time of the
// query only.
// Written from the Vulkan specification (vkCreateInstance, vkEnumeratePhysicalDevices,
// vkGetPhysicalDeviceProperties). No third-party code.
#include <dlfcn.h>
#include <jni.h>
#include <stddef.h>
#include <stdint.h>
#include <vulkan/vulkan.h>

// Returns the driverVersion of the first Vulkan device, or -1 when no Vulkan device answers.
JNIEXPORT jlong JNICALL Java_com_pd2launcher_app_VulkanDriverProbe_nativeDriverVersion(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    jlong version = -1;
    void *lib = dlopen("libvulkan.so", RTLD_NOW | RTLD_LOCAL);
    if (lib == NULL) return -1;
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    PFN_vkCreateInstance create = gipa != NULL ? (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance") : NULL;
    VkApplicationInfo app = {
        .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "pd2-vulkan-driver-probe",
        .apiVersion = VK_API_VERSION_1_0,
    };
    VkInstanceCreateInfo info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance instance = VK_NULL_HANDLE;
    if (create != NULL && create(&info, NULL, &instance) == VK_SUCCESS) {
        PFN_vkDestroyInstance destroy = (PFN_vkDestroyInstance)gipa(instance, "vkDestroyInstance");
        PFN_vkEnumeratePhysicalDevices enumerate =
            (PFN_vkEnumeratePhysicalDevices)gipa(instance, "vkEnumeratePhysicalDevices");
        PFN_vkGetPhysicalDeviceProperties properties =
            (PFN_vkGetPhysicalDeviceProperties)gipa(instance, "vkGetPhysicalDeviceProperties");
        // One device is enough: VK_INCOMPLETE says that there are more.
        uint32_t count = 1;
        VkPhysicalDevice device = VK_NULL_HANDLE;
        VkResult result = enumerate != NULL ? enumerate(instance, &count, &device) : VK_ERROR_INITIALIZATION_FAILED;
        if ((result == VK_SUCCESS || result == VK_INCOMPLETE) && count > 0 && device != VK_NULL_HANDLE &&
            properties != NULL) {
            VkPhysicalDeviceProperties props;
            properties(device, &props);
            version = (jlong)props.driverVersion;
        }
        if (destroy != NULL) destroy(instance, NULL);
    }
    dlclose(lib);
    return version;
}
