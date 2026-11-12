// JNI bindings for se.hellsoft.vaka.internal.NativeCrashes.
#include <jni.h>
#include <signal.h>
#include <stdlib.h>

#include "vaka_crash.h"

#define EXPORT __attribute__((visibility("default")))

EXPORT JNIEXPORT jboolean JNICALL
Java_se_hellsoft_vaka_internal_NativeCrashes_nativeInstall(JNIEnv *env, jclass cls, jstring dir, jstring release) {
    (void)cls;
    const char *d = (*env)->GetStringUTFChars(env, dir, NULL);
    const char *r = release ? (*env)->GetStringUTFChars(env, release, NULL) : NULL;
    jboolean ok = vaka_crash_install(d, r) ? JNI_TRUE : JNI_FALSE;
    if (r) (*env)->ReleaseStringUTFChars(env, release, r);
    (*env)->ReleaseStringUTFChars(env, dir, d);
    return ok;
}

EXPORT JNIEXPORT void JNICALL
Java_se_hellsoft_vaka_internal_NativeCrashes_nativeSetSession(JNIEnv *env, jclass cls, jstring session) {
    (void)cls;
    if (!session) {
        vaka_crash_set_session(NULL);
        return;
    }
    const char *s = (*env)->GetStringUTFChars(env, session, NULL);
    vaka_crash_set_session(s);
    (*env)->ReleaseStringUTFChars(env, session, s);
}

// Deliberately crashes with SIGSEGV, to verify the integration.
EXPORT JNIEXPORT void JNICALL
Java_se_hellsoft_vaka_internal_NativeCrashes_nativeCrash(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    volatile int *p = NULL;
    *p = 42;
    abort();
}
