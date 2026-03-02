package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

public class ReporterConfigDefaultsTest {
    @Test
    void defaultIngestBaseUrlTargetsSeqwawa() {
        ReporterConfig config = new ReporterConfig();
        assertEquals("https://seqwawa.com", config.ingestBaseUrl);
    }

    @Test
    void normalizeIngestBaseUrlInputStripsWildcardsAndTrailingSlashes() {
        assertEquals("https://seqwawa.com", ReporterRuntime.normalizeIngestBaseUrlInput("https://seqwawa.com/*"));
        assertEquals("https://seqwawa.com", ReporterRuntime.normalizeIngestBaseUrlInput(" https://seqwawa.com/ "));
        assertEquals("https://seqwawa.com/api", ReporterRuntime.normalizeIngestBaseUrlInput("https://seqwawa.com/api/***///"));
    }
}
