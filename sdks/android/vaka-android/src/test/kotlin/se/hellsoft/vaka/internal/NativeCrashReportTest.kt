package se.hellsoft.vaka.internal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NativeCrashReportTest {
    companion object {
        /** Output of vaka_crash.c (compiled for the desktop), with the app library moved under /data/app. */
        fun sample(session: String = "11111111-2222-3333-4444-555555555555", end: Boolean = true) = """
            vaka-native-crash 1
            timestamp_ms 1794517033272
            signal 11
            code 1
            fault_addr 0x0
            pid 700
            tid 712
            thread RenderThread
            session $session
            release 1.2.3
            frame 0x5634507793b2
            frame 0x5634507793d4
            frame 0x7fca8162a1ca
            frame 0x7fca8162a28b
            frame 0x1234
            image 0x563450778000 0x15788 20ffbf74c43fb0c055999cc4eed51959fe4ab416 /data/app/~~x/com.example-1/base.apk!/lib/arm64-v8a/libapp.so
            image 0x7fca81894000 0x1522 367b5e28e7feb37dd5f904ebe877bc120eae6015 linux-vdso.so.1
            image 0x7fca81600000 0x212d90 a4a7992a8e66555c8141ab2a08a8465ff6e0ea65 /apex/com.android.runtime/lib64/bionic/libc.so
        """.trimIndent() + if (end) "\nend\n" else "\n"
    }

    @Test fun parsesReport() {
        val r = NativeCrashReport.parse(sample())!!
        assertEquals(11, r.signal)
        assertEquals(1794517033272, r.timestampMillis)
        assertEquals("RenderThread", r.thread)
        assertEquals("11111111-2222-3333-4444-555555555555", r.sessionId)
        assertEquals("1.2.3", r.release)
        assertEquals(5, r.frames.size)
        assertEquals(3, r.images.size)
    }

    @Test fun buildsException() {
        val e = NativeCrashReport.parse(sample())!!.exception()
        assertEquals("SIGSEGV", e["type"])
        assertEquals("Segmentation violation (SEGV_MAPERR) at 0x0", e["value"])
        @Suppress("UNCHECKED_CAST") val frames = e["stacktrace"] as List<Map<String, Any?>>
        assertEquals("0x5634507793b2", frames[0]["instruction_addr"])
        assertEquals(true, frames[0]["in_app"])
        assertEquals("libapp.so+0x13b2", frames[0]["function"])
        assertEquals("/apex/com.android.runtime/lib64/bionic/libc.so", frames[2]["package"])
        assertEquals(false, frames[2]["in_app"])
        assertNull(frames[4]["package"])
        assertNull(frames[4]["function"])
    }

    @Test fun onlyImagesWithFramesAreSent() {
        val images = NativeCrashReport.parse(sample())!!.debugImages()
        assertEquals(listOf("0x563450778000", "0x7fca81600000"), images.map { it["image_addr"] })
        assertEquals("elf", images[0]["type"])
        assertEquals("20ffbf74c43fb0c055999cc4eed51959fe4ab416", images[0]["code_id"])
        assertEquals(0x15788L, images[0]["image_size"])
    }

    @Test fun abortHasNoFaultAddress() {
        val text = sample().replace("signal 11", "signal 6").replace("code 1", "code -6").replace("fault_addr 0x0", "fault_addr 0x2bc")
        val e = NativeCrashReport.parse(text)!!.exception()
        assertEquals("SIGABRT", e["type"])
        assertEquals("Abort (SI_TKILL)", e["value"])
    }

    @Test fun truncatedReportIsStillUsedAndGarbageIsNot() {
        assertEquals(5, NativeCrashReport.parse(sample(end = false))!!.frames.size)
        assertNull(NativeCrashReport.parse("hello"))
        assertNull(NativeCrashReport.parse("vaka-native-crash 1\nframe 0x1\n"))
        assertNull(NativeCrashReport.parse(sample(session = "garbage\u0000!"))!!.sessionId)
    }
}
