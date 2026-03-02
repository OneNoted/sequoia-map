package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

public class GatewayClientTransportPolicyTest {
    @Test
    void blocks_non_localhost_http_when_insecure_override_disabled() {
        String error = GatewayClient.ingestUrlValidationError("http://example.com:3010", false);
        assertEquals("insecure_http_ingest_url_blocked", error);
    }

    @Test
    void allows_localhost_http_for_dev() {
        assertNull(GatewayClient.ingestUrlValidationError("http://127.0.0.1:3010", false));
        assertNull(GatewayClient.ingestUrlValidationError("http://127.42.42.42:3010", false));
        assertNull(GatewayClient.ingestUrlValidationError("http://localhost:3010", false));
    }

    @Test
    void blocks_hostname_prefix_spoof_of_loopback() {
        assertEquals(
            "insecure_http_ingest_url_blocked",
            GatewayClient.ingestUrlValidationError("http://127.example.com:3010", false)
        );
        assertEquals(
            "insecure_http_ingest_url_blocked",
            GatewayClient.ingestUrlValidationError("http://127.0.0.1.nip.io:3010", false)
        );
    }

    @Test
    void allows_non_localhost_http_when_override_enabled() {
        assertNull(GatewayClient.ingestUrlValidationError("http://example.com:3010", true));
    }

    @Test
    void accepts_https_urls() {
        assertNull(GatewayClient.ingestUrlValidationError("https://iris.example.com", false));
    }
}
