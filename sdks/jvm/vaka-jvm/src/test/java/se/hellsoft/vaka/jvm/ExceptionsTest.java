package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;

import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import se.hellsoft.vaka.jvm.internal.Exceptions;

class ExceptionsTest {
    @Test
    void defaultInAppExcludesRuntimeAndFrameworks() {
        Exceptions e = new Exceptions(List.of());
        assertTrue(e.isInApp("com.example.Checkout"));
        assertFalse(e.isInApp("java.util.ArrayList"));
        assertFalse(e.isInApp("org.springframework.web.Servlet"));
        assertFalse(e.isInApp("kotlinx.coroutines.Job"));
        assertFalse(e.isInApp("io.netty.channel.Channel"));
    }

    @Test
    void configuredPackagesAreTheOnlyAppCode() {
        Exceptions e = new Exceptions(List.of("com.example.", " ", "org.acme"));
        assertTrue(e.isInApp("com.example.Checkout"));
        assertTrue(e.isInApp("org.acme.Thing"));
        assertFalse(e.isInApp("com.examples.Other"));
        assertFalse(e.isInApp("net.other.Lib"));
    }

    @Test
    void cyclicCauseChainTerminates() {
        RuntimeException a = new RuntimeException("a");
        RuntimeException b = new RuntimeException("b", a);
        a.initCause(b);
        List<Map<String, Object>> ex = new Exceptions(List.of()).exceptions(b);
        assertEquals(2, ex.size());
    }

    @Test
    void framesAreCrashSiteFirstAndCapped() {
        StackTraceElement[] stack = new StackTraceElement[500];
        for (int i = 0; i < stack.length; i++) stack[i] = new StackTraceElement("com.example.C", "m" + i, "C.java", i + 1);
        RuntimeException t = new RuntimeException();
        t.setStackTrace(stack);
        @SuppressWarnings("unchecked")
        List<Map<String, Object>> frames = (List<Map<String, Object>>) new Exceptions(List.of()).exceptions(t).get(0).get("stacktrace");
        assertEquals(Exceptions.MAX_FRAMES, frames.size());
        assertEquals("m0", frames.get(0).get("function"));
        assertEquals(1, frames.get(0).get("lineno"));
    }
}
