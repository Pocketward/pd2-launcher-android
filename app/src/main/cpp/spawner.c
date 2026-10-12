// The app's process spawner: posix_spawn with
// file actions, a new session, and every inherited descriptor closed on exec but the ones it
// names; a reaper thread per child reports how it ended to com.pd2launcher.app.Spawner.
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static JavaVM *vm;
static jclass spawner_class;
static jmethodID on_exit_method;

JNIEXPORT jint JNI_OnLoad(JavaVM *loaded_vm, void *reserved) {
    (void)reserved;
    vm = loaded_vm;
    JNIEnv *env;
    if ((*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_6) != JNI_OK) return JNI_ERR;
    jclass local = (*env)->FindClass(env, "com/pd2launcher/app/Spawner");
    if (local == NULL) return JNI_ERR;
    spawner_class = (*env)->NewGlobalRef(env, local);
    (*env)->DeleteLocalRef(env, local);
    on_exit_method = (*env)->GetStaticMethodID(env, spawner_class, "onExit", "(IZI)V");
    if (on_exit_method == NULL) return JNI_ERR;
    return JNI_VERSION_1_6;
}

// Waits for one child and reports it: exited (true, status) or killed (false, signal).
static void *reap(void *arg) {
    pid_t pid = (pid_t)(intptr_t)arg;
    int status = 0;
    pid_t waited;
    do {
        waited = waitpid(pid, &status, 0);
    } while (waited < 0 && errno == EINTR);

    jboolean exited = JNI_TRUE;
    jint value = -1;
    if (waited == pid) {
        if (WIFEXITED(status)) {
            value = WEXITSTATUS(status);
        } else if (WIFSIGNALED(status)) {
            exited = JNI_FALSE;
            value = WTERMSIG(status);
        }
    }

    JNIEnv *env;
    JavaVMAttachArgs args = {JNI_VERSION_1_6, "pd2-reaper", NULL};
    if ((*vm)->AttachCurrentThread(vm, &env, &args) == JNI_OK) {
        (*env)->CallStaticVoidMethod(env, spawner_class, on_exit_method, (jint)pid, exited, value);
        if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
        (*vm)->DetachCurrentThread(vm);
    }
    return NULL;
}

// Marks every descriptor of this process above 2 close-on-exec, so a child gets
// only what the file actions give it. Runs in the parent, before the spawn: a
// descriptor another thread opens without O_CLOEXEC in between can still leak.
static void mark_cloexec(void) {
    DIR *dir = opendir("/proc/self/fd");
    if (dir == NULL) return;
    int own = dirfd(dir);
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        char *end;
        long fd = strtol(entry->d_name, &end, 10);
        if (*end != '\0' || end == entry->d_name || fd <= 2 || fd == own) continue;
        int flags = fcntl((int)fd, F_GETFD);
        if (flags >= 0 && !(flags & FD_CLOEXEC)) fcntl((int)fd, F_SETFD, flags | FD_CLOEXEC);
    }
    closedir(dir);
}

// A NULL-terminated copy of a Java String[]; free with free_strings.
static char **copy_strings(JNIEnv *env, jobjectArray array) {
    jsize count = (*env)->GetArrayLength(env, array);
    char **strings = calloc((size_t)count + 1, sizeof(char *));
    if (strings == NULL) return NULL;
    for (jsize i = 0; i < count; i++) {
        jstring string = (jstring)(*env)->GetObjectArrayElement(env, array, i);
        const char *chars = (*env)->GetStringUTFChars(env, string, NULL);
        strings[i] = chars ? strdup(chars) : NULL;
        if (chars) (*env)->ReleaseStringUTFChars(env, string, chars);
        (*env)->DeleteLocalRef(env, string);
        if (strings[i] == NULL) {
            for (jsize j = 0; j < i; j++) free(strings[j]);
            free(strings);
            return NULL;
        }
    }
    return strings;
}

static void free_strings(char **strings) {
    if (strings == NULL) return;
    for (char **s = strings; *s != NULL; s++) free(*s);
    free(strings);
}

// Returns the child's pid, or -errno. [fd3]: a descriptor the child gets as its descriptor 3, or -1.
JNIEXPORT jint JNICALL Java_com_pd2launcher_app_Spawner_nativeSpawn(
    JNIEnv *env, jclass clazz, jstring path, jobjectArray argv, jobjectArray envp, jint stdin_fd,
    jint output_fd, jint fd3) {
    (void)clazz;
    int result = 0;
    pid_t pid = 0;
    // Each copy only while the one before worked: a failed one may leave an exception pending,
    // and no other JNI call may follow it.
    char **child_argv = copy_strings(env, argv);
    char **child_envp = child_argv != NULL ? copy_strings(env, envp) : NULL;
    const char *child_path = child_envp != NULL ? (*env)->GetStringUTFChars(env, path, NULL) : NULL;
    if (child_argv == NULL || child_envp == NULL || child_path == NULL) {
        result = -ENOMEM;
        goto out;
    }
    // A descriptor that already is 3 is copied first: a dup2 onto itself would leave it
    // close-on-exec on Androids whose posix_spawn predates POSIX's rule for that case. Without
    // the copy the child would start without it, so the spawn fails instead.
    int fd3_copy = -1;
    if (fd3 == 3) {
        fd3_copy = fcntl(fd3, F_DUPFD_CLOEXEC, 4);
        if (fd3_copy < 0) {
            result = -errno;
            goto out;
        }
    }

    posix_spawn_file_actions_t actions;
    posix_spawnattr_t attr;
    posix_spawn_file_actions_init(&actions);
    posix_spawnattr_init(&attr);

    // The given descriptors become standard input and both standard output and
    // error (the app drains the latter into the process's log); dup2 clears
    // their close-on-exec in the child.
    posix_spawn_file_actions_adddup2(&actions, stdin_fd, 0);
    posix_spawn_file_actions_adddup2(&actions, output_fd, 1);
    posix_spawn_file_actions_adddup2(&actions, output_fd, 2);
    if (fd3 >= 0) posix_spawn_file_actions_adddup2(&actions, fd3_copy >= 0 ? fd3_copy : fd3, 3);

    // A new session, no blocked signals, and default dispositions: the app's
    // runtime blocks and handles signals the child must not inherit.
    sigset_t empty, defaults;
    sigemptyset(&empty);
    sigemptyset(&defaults);
    for (int s = 1; s < 32; s++) {
        if (s != SIGKILL && s != SIGSTOP) sigaddset(&defaults, s);
    }
    posix_spawnattr_setsigmask(&attr, &empty);
    posix_spawnattr_setsigdefault(&attr, &defaults);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSID | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF);

    mark_cloexec();
    // Children create owner-only files and folders (the app process's umask is 0); posix_spawn
    // has no umask attribute, so the parent's is set around the spawn (the Kotlin side holds
    // its lock, and a file another app thread creates meanwhile is only stricter).
    mode_t umask_before = umask(077);
    result = posix_spawn(&pid, child_path, &actions, &attr, child_argv, child_envp);
    umask(umask_before);
    posix_spawn_file_actions_destroy(&actions);
    if (fd3_copy >= 0) close(fd3_copy);
    posix_spawnattr_destroy(&attr);
    if (result != 0) {
        result = -result;
        goto out;
    }

    pthread_t thread;
    pthread_attr_t thread_attr;
    int thread_result = pthread_attr_init(&thread_attr);
    if (thread_result == 0) {
        pthread_attr_setdetachstate(&thread_attr, PTHREAD_CREATE_DETACHED);
        thread_result = pthread_create(&thread, &thread_attr, reap, (void *)(intptr_t)pid);
        pthread_attr_destroy(&thread_attr);
    }
    if (thread_result != 0) {
        // Without a reaper nobody would wait for the child or report it: a zombie and a
        // callback that never runs. The spawn fails instead, leaving nothing behind.
        kill(pid, SIGKILL);
        while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {
        }
        result = -thread_result;
        goto out;
    }
    result = pid;

out:
    if (child_path) (*env)->ReleaseStringUTFChars(env, path, child_path);
    free_strings(child_argv);
    free_strings(child_envp);
    return result;
}

// A connected AF_UNIX stream pair, both ends close-on-exec (the child's end
// reaches the child only through the dup2 to its standard input). Returns
// [app end, child end], or null with errno lost.
JNIEXPORT jintArray JNICALL Java_com_pd2launcher_app_Spawner_nativeSocketPair(JNIEnv *env,
                                                                             jclass clazz) {
    (void)clazz;
    int fds[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds) != 0) return NULL;
    jintArray result = (*env)->NewIntArray(env, 2);
    if (result == NULL) {
        close(fds[0]);
        close(fds[1]);
        return NULL;
    }
    jint values[2] = {fds[0], fds[1]};
    (*env)->SetIntArrayRegion(env, result, 0, 2, values);
    return result;
}

// A new, empty memfd named [name], close-on-exec (a child gets it only as its descriptor 3), that
// can grow but never shrink (F_SEAL_SHRINK): the app maps it, and a process of the app's user that
// reached it through /proc and cut it shorter would end the app with SIGBUS on its next read;
// Xvfb only grows it to the framebuffer's size. Returns the descriptor, or -errno.
JNIEXPORT jint JNICALL Java_com_pd2launcher_app_Spawner_nativeMemfd(JNIEnv *env, jclass clazz, jstring name) {
    (void)clazz;
    const char *chars = (*env)->GetStringUTFChars(env, name, NULL);
    if (chars == NULL) return -ENOMEM;
    int fd = memfd_create(chars, MFD_CLOEXEC | MFD_ALLOW_SEALING);
    int error = errno;
    (*env)->ReleaseStringUTFChars(env, name, chars);
    if (fd >= 0 && fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK) != 0) {
        error = errno;
        close(fd);
        fd = -1;
    }
    return fd >= 0 ? fd : -error;
}
