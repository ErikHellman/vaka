package se.hellsoft.vaka.internal

import java.io.File

/** JNI bridge to the native signal handler in `libvaka.so`. */
internal object NativeCrashes {
    @Volatile private var loaded = false

    /** Loads the library and installs the handler; false if either fails (e.g. no lib for this ABI). */
    fun install(dir: File, release: String, log: Logger): Boolean {
        return try {
            if (!loaded) {
                System.loadLibrary("vaka")
                loaded = true
            }
            dir.mkdirs()
            nativeInstall(dir.absolutePath, release)
        } catch (e: Throwable) {
            log.warn("native crash capture unavailable", e)
            false
        }
    }

    fun setSession(id: String?) {
        if (loaded) nativeSetSession(id)
    }

    /** Crashes the process with SIGSEGV in native code. */
    fun crash() {
        if (!loaded) System.loadLibrary("vaka")
        nativeCrash()
    }

    @JvmStatic private external fun nativeInstall(dir: String, release: String?): Boolean
    @JvmStatic private external fun nativeSetSession(id: String?)
    @JvmStatic private external fun nativeCrash()
}
