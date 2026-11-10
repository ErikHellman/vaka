package se.hellsoft.vaka.internal

import org.junit.Assert.assertEquals
import org.junit.Test

class JsonTest {
    @Test fun writesNestedValues() {
        val s = Json.write(linkedMapOf("a" to 1, "b" to listOf(true, null, 2.5), "c" to mapOf("d" to "x\"y\n\u0001")))
        assertEquals("{\"a\":1,\"b\":[true,null,2.5],\"c\":{\"d\":\"x\\\"y\\n\\u0001\"}}", s)
    }

    @Test fun nanBecomesNull() {
        assertEquals("[null]", Json.write(listOf(Double.NaN)))
    }
}
