package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

public class LegacyMessageScraperTest {
    @Test
    void parsesCaptureSignalFromServerMessage() {
        LegacyMessageScraper.CaptureSignal signal = LegacyMessageScraper.parseCapture(
            "Nivla Woods was captured by [SEQ]",
            1_700_000_000_000L
        );

        assertNotNull(signal);
        assertEquals("Nivla Woods", signal.territory());
        assertEquals("SEQ", signal.guildPrefix());
    }

    @Test
    void parsesQueuedWarSignal() {
        LegacyMessageScraper.WarSignal signal = LegacyMessageScraper.parseWar(
            "Nivla Woods is under attack",
            1_700_000_000_000L
        );

        assertNotNull(signal);
        assertEquals("Nivla Woods", signal.territory());
        assertEquals("queued", signal.kind());
    }

    @Test
    void ignoresUnrelatedMessages() {
        assertNull(LegacyMessageScraper.parseCapture("welcome to wynncraft", 1_700_000_000_000L));
        assertNull(LegacyMessageScraper.parseWar("welcome to wynncraft", 1_700_000_000_000L));
    }
}
