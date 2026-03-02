package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class GatewayClientSubmitResponsePolicyTest {
    @Test
    void successfulAckWithAcceptedUpdatesIsOk() {
        GatewayClient.SubmitResult result = GatewayClient.interpretSubmitResponse(
            200,
            "{\"ok\":true,\"accepted\":2,\"rejected\":0}"
        );

        assertTrue(result.ok);
        assertTrue(result.terminal);
        assertEquals(2, result.accepted);
        assertEquals(0, result.rejected);
    }

    @Test
    void allRejectedAckIsTerminalRejection() {
        GatewayClient.SubmitResult result = GatewayClient.interpretSubmitResponse(
            200,
            "{\"ok\":true,\"accepted\":0,\"rejected\":3}"
        );

        assertFalse(result.ok);
        assertFalse(result.unauthorized);
        assertTrue(result.terminal);
        assertEquals(0, result.accepted);
        assertEquals(3, result.rejected);
        assertEquals("upload_rejected_all", result.error);
    }

    @Test
    void malformedAckFallsBackToLegacySuccessHandling() {
        GatewayClient.SubmitResult result = GatewayClient.interpretSubmitResponse(200, "not-json");

        assertTrue(result.ok);
        assertTrue(result.terminal);
        assertEquals(0, result.accepted);
        assertEquals(0, result.rejected);
    }

    @Test
    void non2xxStillFailsWithTransportError() {
        GatewayClient.SubmitResult result = GatewayClient.interpretSubmitResponse(503, "{}");

        assertFalse(result.ok);
        assertFalse(result.terminal);
        assertFalse(result.unauthorized);
        assertEquals("gateway_http_503", result.error);
    }

    @Test
    void payloadTooLargeIsTerminalRejection() {
        GatewayClient.SubmitResult result = GatewayClient.interpretSubmitResponse(413, "{}");

        assertFalse(result.ok);
        assertTrue(result.terminal);
        assertFalse(result.unauthorized);
        assertEquals("gateway_http_413", result.error);
    }
}
