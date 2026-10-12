// A full memory barrier for the gamepad file's seqlock (input/Gamepad.kt): the game's
// xinput1_4.dll reads the mapped file from another process, and the app's minimum API level
// has no VarHandle fences.
#include <jni.h>

JNIEXPORT void JNICALL Java_com_pd2launcher_app_input_GamepadFile_nativeFence(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
}
