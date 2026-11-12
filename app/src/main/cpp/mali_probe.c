// The device check's Mali probe: reads the GPU id from Arm's mali_kbase driver, before anything
// is unpacked, as the App userland's PanVK drivers open it. kbase answers GET_GPUPROPS only on a
// set-up context, so the probe does the handshake first (VERSION_CHECK, SET_FLAGS). The CSF
// handshake comes first, as our PanVK backend makes it
// (userland/mesa-kbase/src/panfrost/lib/kmod/kbase_kmod.c, kbase_kmod_dev_create). A job-manager
// (pre-CSF) kbase refuses that number before its own handshake (EPERM) or answers it with its own
// major 11; the probe then makes the job-manager handshake as the mali-panvk tree's kbase backend,
// the driver of the job-manager Malis, makes it (src/panfrost/lib/kmod/kbase_kmod.c at config.sh's
// MALI_PANVK_COMMIT, kbase_kmod_dev_create: number 0, 11.31, after a refused CSF one). A CSF kbase
// that answers the CSF handshake never gets the second one.
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

// The interface, from Arm's kbase uAPI headers as the PanVK build fetches them (KBASE_UAPI_* in
// userland/config.sh): mali_kbase_ioctl.h (KBASE_IOCTL_TYPE,
// struct kbase_ioctl_set_flags and KBASE_IOCTL_SET_FLAGS, struct kbase_ioctl_get_gpuprops and
// KBASE_IOCTL_GET_GPUPROPS, KBASE_GPUPROP_*), csf/mali_kbase_csf_ioctl.h
// (struct kbase_ioctl_version_check, KBASE_IOCTL_VERSION_CHECK, BASE_UK_VERSION_MAJOR/MINOR, and
// KBASE_IOCTL_VERSION_CHECK_RESERVED: number 0, the job manager's handshake).
#define KBASE_IOCTL_TYPE 0x80

struct kbase_ioctl_version_check {
    uint16_t major;
    uint16_t minor;
};
#define KBASE_IOCTL_VERSION_CHECK _IOWR(KBASE_IOCTL_TYPE, 52, struct kbase_ioctl_version_check)
#define KBASE_IOCTL_JM_VERSION_CHECK _IOWR(KBASE_IOCTL_TYPE, 0, struct kbase_ioctl_version_check)

struct kbase_ioctl_set_flags {
    uint32_t create_flags;
};
#define KBASE_IOCTL_SET_FLAGS _IOW(KBASE_IOCTL_TYPE, 1, struct kbase_ioctl_set_flags)

struct kbase_ioctl_get_gpuprops {
    uint64_t buffer;
    uint32_t size;
    uint32_t flags;
};
#define KBASE_IOCTL_GET_GPUPROPS _IOW(KBASE_IOCTL_TYPE, 3, struct kbase_ioctl_get_gpuprops)
#define KBASE_GPUPROP_PRODUCT_ID 1
#define KBASE_GPUPROP_VERSION_STATUS 2
#define KBASE_GPUPROP_MINOR_REVISION 3
#define KBASE_GPUPROP_MAJOR_REVISION 4
#define KBASE_GPUPROP_RAW_GPU_ID 55

struct kbase_ioctl_get_ddk_version {
    uint64_t version_buffer;
    uint32_t size;
    uint32_t padding;
};
#define KBASE_IOCTL_GET_DDK_VERSION _IOW(KBASE_IOCTL_TYPE, 13, struct kbase_ioctl_get_ddk_version)

// The uAPI the probe offers (BASE_UK_VERSION_*): kbase answers the lower minor of its own and this
// one, or its own version when the major differs.
#define CSF_UAPI_MAJOR 1
#define CSF_UAPI_MINOR 38
// The job manager's, as mali-panvk offers it.
#define JM_UAPI_MAJOR 11
#define JM_UAPI_MINOR 31

// One property of a kbase property blob: (key << 2 | size code) words, each followed by its value
// of 1 << size code bytes, as our backend reads them (kbase_kmod.c, kbase_read_props); 0 when the
// blob does not have it.
static uint64_t prop_from(const uint8_t *buf, int len, uint32_t want) {
    for (int off = 0; off + 4 <= len;) {
        uint32_t key;
        memcpy(&key, buf + off, 4);
        off += 4;
        unsigned bytes = 1u << (key & 3);
        if (off + (int)bytes > len) break;
        uint64_t value = 0;
        memcpy(&value, buf + off, bytes);
        off += bytes;
        if ((key >> 2) == want) return value;
    }
    return 0;
}

// The GPU id on the job manager as mali-panvk makes it (kbase_kmod_query_props): the GPU_ID
// register from its parts, with the product id in bits 16 to 31 (Bifrost and Valhall: arch major,
// arch minor, arch rev, product major), so that the arch and the model read as on CSF.
static uint64_t jm_gpu_id_from(const uint8_t *buf, int len) {
    uint64_t product = prop_from(buf, len, KBASE_GPUPROP_PRODUCT_ID);
    if (product == 0) return 0;
    return (product << 16) | ((prop_from(buf, len, KBASE_GPUPROP_MAJOR_REVISION) & 0xf) << 12) |
        ((prop_from(buf, len, KBASE_GPUPROP_MINOR_REVISION) & 0xff) << 4) |
        (prop_from(buf, len, KBASE_GPUPROP_VERSION_STATUS) & 0xf);
}

// The handshake on a new /dev/mali0 context: the CSF one, after a refusal the job manager's, then
// SET_FLAGS. Returns 0 (with the agreed [version]) or an errno (ENOTTY: neither a CSF nor a
// job-manager kbase; when both handshakes fail, the CSF one's errno).
static int set_up(int fd, struct kbase_ioctl_version_check *version) {
    *version = (struct kbase_ioctl_version_check){.major = CSF_UAPI_MAJOR, .minor = CSF_UAPI_MINOR};
    if (ioctl(fd, KBASE_IOCTL_VERSION_CHECK, version) < 0) {
        int csf_errno = errno;
        *version = (struct kbase_ioctl_version_check){.major = JM_UAPI_MAJOR, .minor = JM_UAPI_MINOR};
        if (ioctl(fd, KBASE_IOCTL_JM_VERSION_CHECK, version) < 0) {
            *version = (struct kbase_ioctl_version_check){0};
            return csf_errno;
        }
    }
    if (version->major != CSF_UAPI_MAJOR && version->major != JM_UAPI_MAJOR) return ENOTTY;
    struct kbase_ioctl_set_flags flags = {0};
    return ioctl(fd, KBASE_IOCTL_SET_FLAGS, &flags) < 0 ? errno : 0;
}

// Returns {errno (0 on success; ENOTTY: neither a CSF nor a job-manager kbase), gpu_id, uapi major
// (1: CSF, 11: the job manager), uapi minor}.
JNIEXPORT jlongArray JNICALL Java_com_pd2launcher_app_MaliProbe_nativeProbe(JNIEnv *env, jclass cls) {
    (void)cls;
    jlong result[4] = {0, 0, 0, 0};
    int fd = open("/dev/mali0", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        result[0] = errno;
        goto out;
    }
    struct kbase_ioctl_version_check version;
    int err = set_up(fd, &version);
    result[2] = version.major;
    result[3] = version.minor;
    if (err) {
        result[0] = err;
        goto close;
    }
    struct kbase_ioctl_get_gpuprops props = {0};
    int size = ioctl(fd, KBASE_IOCTL_GET_GPUPROPS, &props);
    if (size <= 0) {
        result[0] = size < 0 ? errno : EIO;
        goto close;
    }
    uint8_t *buf = calloc(1, (size_t)size);
    if (buf == NULL) {
        result[0] = ENOMEM;
        goto close;
    }
    props.buffer = (uintptr_t)buf;
    props.size = (uint32_t)size;
    int got = ioctl(fd, KBASE_IOCTL_GET_GPUPROPS, &props);
    if (got < 0) {
        result[0] = errno;
    } else {
        uint64_t id = version.major == JM_UAPI_MAJOR ? jm_gpu_id_from(buf, got) : prop_from(buf, got, KBASE_GPUPROP_RAW_GPU_ID);
        result[1] = (jlong)id;
        if (id == 0) result[0] = EIO;
    }
    free(buf);
close:
    close(fd);
out:;
    jlongArray array = (*env)->NewLongArray(env, 4);
    if (array != NULL) (*env)->SetLongArrayRegion(env, array, 0, 4, result);
    return array;
}

// The kernel driver's release as kbase names it (GET_DDK_VERSION, e.g. "K:r44p1-01eac0(GPL)"), or
// null when the node or the call does not answer. Kernels of one uAPI version differ by release
// (issue 23: a MediaTek r32p1 kbase reports uAPI 1.18 but lacks calls and layouts of 1.18).
JNIEXPORT jstring JNICALL Java_com_pd2launcher_app_MaliProbe_nativeDdkVersion(JNIEnv *env, jclass cls) {
    (void)cls;
    char text[128] = {0};
    int fd = open("/dev/mali0", O_RDWR | O_CLOEXEC);
    if (fd < 0) return NULL;
    struct kbase_ioctl_version_check version;
    struct kbase_ioctl_get_ddk_version req = {.version_buffer = (uintptr_t)text, .size = sizeof(text) - 1};
    int got = set_up(fd, &version) == 0 ? ioctl(fd, KBASE_IOCTL_GET_DDK_VERSION, &req) : -1;
    close(fd);
    return got > 0 ? (*env)->NewStringUTF(env, text) : NULL;
}
