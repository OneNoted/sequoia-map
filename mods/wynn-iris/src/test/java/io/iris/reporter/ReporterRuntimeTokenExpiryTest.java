package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.time.Instant;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class ReporterRuntimeTokenExpiryTest {
    @Test
    void missingTokenIsExpired() {
        Instant now = Instant.parse("2026-01-01T00:00:00Z");
        assertTrue(ReporterRuntime.isTokenMissingOrExpired(null, null, now));
    }

    @Test
    void tokenWithoutExpiryIsTreatedAsPresent() {
        Instant now = Instant.parse("2026-01-01T00:00:00Z");
        assertFalse(ReporterRuntime.isTokenMissingOrExpired("abc", "", now));
    }

    @Test
    void futureExpiryIsNotExpired() {
        Instant now = Instant.parse("2026-01-01T00:00:00Z");
        assertFalse(ReporterRuntime.isTokenMissingOrExpired("abc", "2026-01-01T00:10:00Z", now));
    }

    @Test
    void nearExpiryIsExpired() {
        Instant now = Instant.parse("2026-01-01T00:00:00Z");
        assertTrue(ReporterRuntime.isTokenMissingOrExpired("abc", "2026-01-01T00:00:15Z", now));
    }

    @Test
    void malformedExpiryFailsClosed() {
        Instant now = Instant.parse("2026-01-01T00:00:00Z");
        assertTrue(ReporterRuntime.isTokenMissingOrExpired("abc", "not-an-instant", now));
    }
}
