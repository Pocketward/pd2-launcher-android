// The device check's GPU probe: reads the Adreno chip id
// from the KGSL driver and maps it to a generation, before anything is unpacked.
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/ioctl.h>
#include <unistd.h>

// From the Linux UAPI header include/uapi/linux/msm_kgsl.h as published by Qualcomm in the msm
// kernel trees (CodeLinaro/AOSP kernel/msm) and carried by Mesa as src/freedreno/vulkan/msm_kgsl.h:
// KGSL_IOC_TYPE, struct kgsl_device_getproperty, IOCTL_KGSL_DEVICE_GETPROPERTY,
// KGSL_PROP_DEVICE_INFO and struct kgsl_devinfo.
#define KGSL_IOC_TYPE 0x09
#define KGSL_PROP_DEVICE_INFO 0x1

struct kgsl_device_getproperty {
    unsigned int type;
    void *value;
    size_t sizebytes;
};

#define IOCTL_KGSL_DEVICE_GETPROPERTY _IOWR(KGSL_IOC_TYPE, 0x2, struct kgsl_device_getproperty)

struct kgsl_devinfo {
    unsigned int device_id;
    unsigned int chip_id; // coreid:8 majorrev:8 minorrev:8 patch:8
    unsigned int mmu_enabled;
    unsigned long gmem_gpubaseaddr;
    unsigned int gpu_id;
    size_t gmem_sizebytes;
};

// Adreno generation (6, 7 or 8; another number for older or unknown cores) from the core byte.
// Up to the A730 the core byte is the generation (0x06030001 is the A630, 0x07030001 the A730);
// later KGSL chips use 0x43 for the 7xx line (0x43050a01 is the A740, 0x43051401 the A750) and
// 0x44 for the 8xx line (0x44050001 is the A830), per Mesa's src/freedreno/common/freedreno_devices.py.
static int generation(uint32_t chip_id) {
    uint32_t core = chip_id >> 24;
    if (core == 0x43) return 7;
    if (core == 0x44) return 8;
    return (int)core;
}

// Returns {errno (0 on success), chip_id, generation}.
JNIEXPORT jlongArray JNICALL Java_com_pd2launcher_app_KgslProbe_nativeProbe(JNIEnv *env, jclass cls) {
    (void)cls;
    jlong result[3] = {0, 0, 0};
    int fd = open("/dev/kgsl-3d0", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        result[0] = errno;
    } else {
        struct kgsl_devinfo info = {0};
        struct kgsl_device_getproperty property = {
            .type = KGSL_PROP_DEVICE_INFO,
            .value = &info,
            .sizebytes = sizeof(info),
        };
        if (ioctl(fd, IOCTL_KGSL_DEVICE_GETPROPERTY, &property) < 0) {
            result[0] = errno;
        } else {
            result[1] = (jlong)info.chip_id;
            result[2] = generation(info.chip_id);
        }
        close(fd);
    }
    jlongArray array = (*env)->NewLongArray(env, 3);
    if (array != NULL) (*env)->SetLongArrayRegion(env, array, 0, 3, result);
    return array;
}
