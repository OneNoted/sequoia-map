package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class ReporterConfigDefaultsTest {
    @Test
    void defaultIngestBaseUrlTargetsSeqwawa() {
        ReporterConfig config = new ReporterConfig();
        assertEquals("https://map.seqwawa.com", config.ingestBaseUrl);
    }

    @Test
    void normalizeIngestBaseUrlInputStripsWildcardsAndTrailingSlashes() {
        assertEquals(
                "https://map.seqwawa.com",
                ReporterRuntime.normalizeIngestBaseUrlInput("https://map.seqwawa.com/*"));
        assertEquals(
                "https://map.seqwawa.com",
                ReporterRuntime.normalizeIngestBaseUrlInput(" https://map.seqwawa.com/ "));
        assertEquals(
                "https://map.seqwawa.com/api",
                ReporterRuntime.normalizeIngestBaseUrlInput("https://map.seqwawa.com/api/***///"));
    }

    @Test
    void defaultAutoUpdateSettingsAreEnabledAndPinnedToPublicRepo() {
        ReporterConfig config = new ReporterConfig();
        assertTrue(config.autoUpdateEnabled);
        assertEquals("OneNoted/sequoia-map", config.autoUpdateRepo);
        assertFalse(config.autoUpdateIncludePrerelease);
        assertEquals("never", config.autoUpdateLastResult);
    }
}
