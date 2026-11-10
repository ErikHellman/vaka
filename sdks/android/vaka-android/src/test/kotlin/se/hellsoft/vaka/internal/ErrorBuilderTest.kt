package se.hellsoft.vaka.internal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ErrorBuilderTest {
    private val b = ErrorBuilder(listOf("com.example"))

    @Test fun causeChainOutermostFirstAndCrashSiteFirst() {
        val root = IllegalArgumentException("root")
        val outer = IllegalStateException("outer", root)
        val ex = b.exceptions(outer)
        assertEquals(2, ex.size)
        assertEquals("java.lang.IllegalStateException", ex[0]["type"])
        assertEquals("outer", ex[0]["value"])
        assertEquals("java.lang.IllegalArgumentException", ex[1]["type"])
        @Suppress("UNCHECKED_CAST")
        val frames = ex[0]["stacktrace"] as List<Map<String, Any?>>
        assertEquals("causeChainOutermostFirstAndCrashSiteFirst", frames[0]["function"])
        assertEquals(ErrorBuilderTest::class.java.name, frames[0]["module"])
    }

    @Test fun cyclicCausesTerminate() {
        val a = RuntimeException("a")
        val c = RuntimeException("c", a)
        a.initCause(c)
        assertEquals(2, b.exceptions(c).size)
    }

    @Test fun inApp() {
        assertTrue(b.isInApp("com.example.cart.CartViewModel"))
        assertTrue(b.isInApp("a.b.c")) // R8-obfuscated
        assertFalse(b.isInApp("android.view.View"))
        assertFalse(b.isInApp("kotlinx.coroutines.DispatchedTask"))
        assertFalse(b.isInApp("se.hellsoft.vaka.Vaka"))
    }
}
