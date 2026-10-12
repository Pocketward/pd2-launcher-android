// The device check's Mali probe: reads the GPU id from Arm's mali_kbase driver, before anything
// is unpacked, as our PanVK backend opens it
// (userland/mesa-kbase/src/panfrost/lib/kmod/kbase_kmod.c, kbase_kmod_dev_create). kbase
// answers GET_GPUPROPS only on a set-up context, so the probe does the CSF handshake first
// (VERSION_CHECK, SET_FLAGS). A job-manager (pre-CSF) kbase has its handshake under another
// number (0, which CSF reserves) and answers uAPI major 11, so the probe fails there: not CSF.
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
// KBASE_IOCTL_GET_GPUPROPS, KBASE_GPUPROP_RAW_GPU_ID), csf/mali_kbase_csf_ioctl.h
// (struct kbase_ioctl_version_check, KBASE_IOCTL_VERSION_CHECK, BASE_UK_VERSION_MAJOR/MINOR).
#define KBASE_IOCTL_TYPE 0x80

struct kbase_ioctl_version_check {
    uint16_t major;
    uint16_t minor;
};
#define KBASE_IOCTL_VERSION_CHECK _IOWR(KBASE_IOCTL_TYPE, 52, struct kbase_ioctl_version_check)

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
#define KBASE_GPUPROP_RAW_GPU_ID 55

// The uAPI the probe offers (BASE_UK_VERSION_*): kbase answers the lower minor of its own and this
// one, or its own version when the major differs.
#define CSF_UAPI_MAJOR 1
#define CSF_UAPI_MINOR 38

// The GPU id in a kbase property blob: (key << 2 | size code) words, each followed by its value of
// 1 << size code bytes, as our backend reads them (kbase_kmod.c, kbase_read_props).
static uint64_t gpu_id_from(const uint8_t *buf, int len) {
    for (int off = 0; off + 4 <= len;) {
        uint32_t key;
        memcpy(&key, buf + off, 4);
        off += 4;
        unsigned bytes = 1u << (key & 3);
        if (off + (int)bytes > len) break;
        uint64_t value = 0;
        memcpy(&value, buf + off, bytes);
        off += bytes;
        if ((key >> 2) == KBASE_GPUPROP_RAW_GPU_ID) return value;
    }
    return 0;
}

// Returns {errno (0 on success; ENOTTY: not a CSF kbase), gpu_id, uapi major, uapi minor}.
JNIEXPORT jlongArray JNICALL Java_com_pd2launcher_app_MaliProbe_nativeProbe(JNIEnv *env, jclass cls) {
    (void)cls;
    jlong result[4] = {0, 0, 0, 0};
    int fd = open("/dev/mali0", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        result[0] = errno;
        goto out;
    }
    struct kbase_ioctl_version_check version = {.major = CSF_UAPI_MAJOR, .minor = CSF_UAPI_MINOR};
    if (ioctl(fd, KBASE_IOCTL_VERSION_CHECK, &version) < 0) {
        result[0] = errno;
        goto close;
    }
    result[2] = version.major;
    result[3] = version.minor;
    if (version.major != CSF_UAPI_MAJOR) {
        result[0] = ENOTTY;
        goto close;
    }
    struct kbase_ioctl_set_flags flags = {0};
    if (ioctl(fd, KBASE_IOCTL_SET_FLAGS, &flags) < 0) {
        result[0] = errno;
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
        result[1] = (jlong)gpu_id_from(buf, got);
        if (result[1] == 0) result[0] = EIO;
    }
    free(buf);
close:
    close(fd);
out:;
    jlongArray array = (*env)->NewLongArray(env, 4);
    if (array != NULL) (*env)->SetLongArrayRegion(env, array, 0, 4, result);
    return array;
}
