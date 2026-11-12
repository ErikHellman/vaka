# Vaka uses no reflection. Upload your mapping file with the se.hellsoft.vaka Gradle plugin so
# stack traces are deobfuscated server-side.
# JNI entry points of libvaka.so are looked up by name.
-keepclasseswithmembernames class se.hellsoft.vaka.internal.NativeCrashes {
    native <methods>;
}
