package se.hellsoft.vaka.jvm;

import static org.junit.jupiter.api.Assertions.*;

import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import se.hellsoft.vaka.jvm.internal.Json;

class JsonTest {
    @Test
    void roundTrips() {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("s", "quote \" backslash \\ newline \n tab \t unicode å ctrl \u0001");
        m.put("i", 42);
        m.put("d", 1.5);
        m.put("whole", 3.0);
        m.put("nan", Double.NaN);
        m.put("b", true);
        m.put("n", null);
        m.put("l", Arrays.asList(1, "two", false));
        String json = Json.write(m);
        assertTrue(json.contains("\"whole\":3,"));
        assertTrue(json.contains("\\u0001"));
        @SuppressWarnings("unchecked")
        Map<String, Object> back = (Map<String, Object>) Json.parse(json);
        assertEquals(m.get("s"), back.get("s"));
        assertEquals(42L, back.get("i"));
        assertEquals(1.5, back.get("d"));
        assertNull(back.get("nan"));
        assertEquals(true, back.get("b"));
        assertTrue(back.containsKey("n"));
        assertEquals(List.of(1L, "two", false), back.get("l"));
    }

    @Test
    void parsesEscapesAndWhitespace() {
        assertEquals(Map.of("a", List.of(), "b", Map.of(), "c", "å/x"), Json.parse(" {\"a\" : [ ], \"b\":{}, \"c\":\"\\u00e5\\/x\"} "));
        assertEquals(-1.5e3, Json.parse("-1.5e3"));
    }

    @Test
    void rejectsMalformed() {
        for (String bad : new String[] {"", "{", "[1,]", "{\"a\" 1}", "tru", "\"x", "1 2", "{\"a\":1,}"}) {
            assertThrows(IllegalArgumentException.class, () -> Json.parse(bad), bad);
        }
    }
}
