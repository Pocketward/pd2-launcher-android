/* Stand-in for Android's <android/log.h> in a glibc build of the radv-xclipse tree and of the
 * mali-panvk tree, which has radv-xclipse's util and Vulkan runtime (build-radv.sh and
 * build-mali-panvk.sh put this folder at include/xclipse_linux_compat, which the trees'
 * meson.build adds for glibc Linux builds; neither tree at its pinned commit carries the folder).
 * Logs go to stderr. */
#ifndef PD2_XCLIPSE_COMPAT_ANDROID_LOG_H
#define PD2_XCLIPSE_COMPAT_ANDROID_LOG_H

#include <stdarg.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum android_LogPriority {
   ANDROID_LOG_UNKNOWN = 0,
   ANDROID_LOG_DEFAULT,
   ANDROID_LOG_VERBOSE,
   ANDROID_LOG_DEBUG,
   ANDROID_LOG_INFO,
   ANDROID_LOG_WARN,
   ANDROID_LOG_ERROR,
   ANDROID_LOG_FATAL,
   ANDROID_LOG_SILENT,
} android_LogPriority;

static inline int
__android_log_vprint(int prio, const char *tag, const char *fmt, va_list ap)
{
   (void)prio;
   int n = fprintf(stderr, "%s: ", tag ? tag : "radv");
   n += vfprintf(stderr, fmt, ap);
   n += fprintf(stderr, "\n");
   return n;
}

static inline int __attribute__((format(printf, 3, 4)))
__android_log_print(int prio, const char *tag, const char *fmt, ...)
{
   va_list ap;
   va_start(ap, fmt);
   int n = __android_log_vprint(prio, tag, fmt, ap);
   va_end(ap);
   return n;
}

static inline int
__android_log_write(int prio, const char *tag, const char *text)
{
   (void)prio;
   return fprintf(stderr, "%s: %s\n", tag ? tag : "radv", text ? text : "");
}

#ifdef __cplusplus
}
#endif

#endif
