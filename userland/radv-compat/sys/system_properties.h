/* Stand-in for Android's <sys/system_properties.h> in a glibc build of the radv-xclipse and
 * mali-panvk trees (see ../android/log.h). A glibc process has no Android properties: every read
 * finds none, so the trees' switches come from their environment variables only (the trees read
 * those first). */
#ifndef PD2_XCLIPSE_COMPAT_SYSTEM_PROPERTIES_H
#define PD2_XCLIPSE_COMPAT_SYSTEM_PROPERTIES_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PROP_NAME_MAX 32
#define PROP_VALUE_MAX 92

typedef struct prop_info prop_info;

static inline int
__system_property_get(const char *name, char *value)
{
   (void)name;
   if (value)
      value[0] = '\0';
   return 0;
}

static inline const prop_info *
__system_property_find(const char *name)
{
   (void)name;
   return (const prop_info *)0;
}

static inline void
__system_property_read_callback(const prop_info *pi,
                                void (*callback)(void *cookie, const char *name,
                                                 const char *value, uint32_t serial),
                                void *cookie)
{
   (void)pi;
   (void)callback;
   (void)cookie;
}

static inline int
__system_property_foreach(void (*propfn)(const prop_info *pi, void *cookie), void *cookie)
{
   (void)propfn;
   (void)cookie;
   return 0;
}

#ifdef __cplusplus
}
#endif

#endif
