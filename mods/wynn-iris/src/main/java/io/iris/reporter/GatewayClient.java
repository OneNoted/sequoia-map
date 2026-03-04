package io.iris.reporter;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.Locale;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class GatewayClient {
    private static final Gson GSON = new GsonBuilder().create();

    private final HttpClient httpClient = HttpClient.newBuilder()
        .connectTimeout(Duration.ofSeconds(5))
        .build();
    private final ExecutorService requestExecutor = Executors.newSingleThreadExecutor(r -> {
        Thread thread = new Thread(r, "wynn-iris-net");
        thread.setDaemon(true);
        return thread;
    });

    public CompletableFuture<EnrollResult> enrollAsync(ReporterConfig config, String validityState) {
        String ingestBaseUrl = config.ingestBaseUrl;
        boolean allowInsecureIngestHttp = config.allowInsecureIngestHttp;
        String reporterId = config.reporterId;
        boolean guildOptIn = config.guildOptIn;
        GatewayModels.FieldToggles toggles = GatewayModels.fromConfig(config);
        String minecraftVersion = RuntimeVersionResolver.currentMinecraftVersion();
        String modVersion = RuntimeVersionResolver.currentModVersion();
        String validity = validityState;

        return CompletableFuture.supplyAsync(
            () -> enroll(
                ingestBaseUrl,
                allowInsecureIngestHttp,
                reporterId,
                guildOptIn,
                toggles,
                minecraftVersion,
                modVersion,
                validity,
                config
            ),
            requestExecutor
        );
    }

    public CompletableFuture<HeartbeatResult> heartbeatAsync(ReporterConfig config, String validityState) {
        String ingestBaseUrl = config.ingestBaseUrl;
        boolean allowInsecureIngestHttp = config.allowInsecureIngestHttp;
        String token = config.token;
        boolean guildOptIn = config.guildOptIn;
        GatewayModels.FieldToggles toggles = GatewayModels.fromConfig(config);
        String validity = validityState;
        return CompletableFuture.supplyAsync(
            () -> heartbeat(ingestBaseUrl, allowInsecureIngestHttp, token, guildOptIn, toggles, validity, config),
            requestExecutor
        );
    }

    public CompletableFuture<SubmitResult> submitTerritoryBatchAsync(
        ReporterConfig config,
        GatewayModels.TerritoryBatch batch,
        String validityState
    ) {
        String ingestBaseUrl = config.ingestBaseUrl;
        boolean allowInsecureIngestHttp = config.allowInsecureIngestHttp;
        String token = config.token;
        String validity = validityState;
        return CompletableFuture.supplyAsync(
            () -> postAuthed(ingestBaseUrl, allowInsecureIngestHttp, token, "/v1/report/territory", batch, validity, config),
            requestExecutor
        );
    }

    public static final class EnrollResult {
        public final boolean ok;
        public final String reporterId;
        public final String token;
        public final String tokenExpiresAt;
        public final boolean guildOptIn;
        public final GatewayModels.FieldToggles fieldToggles;
        public final String error;

        private EnrollResult(
            boolean ok,
            String reporterId,
            String token,
            String tokenExpiresAt,
            boolean guildOptIn,
            GatewayModels.FieldToggles fieldToggles,
            String error
        ) {
            this.ok = ok;
            this.reporterId = reporterId;
            this.token = token;
            this.tokenExpiresAt = tokenExpiresAt;
            this.guildOptIn = guildOptIn;
            this.fieldToggles = fieldToggles;
            this.error = error;
        }

        public static EnrollResult failed() {
            return failed("enroll_failed");
        }

        public static EnrollResult failed(String error) {
            return new EnrollResult(false, null, null, null, false, null, normalizeError(error));
        }
    }

    public static final class HeartbeatResult {
        public final boolean ok;
        public final boolean unauthorized;
        public final String tokenExpiresAt;
        public final String rotatedToken;
        public final boolean guildOptIn;
        public final GatewayModels.FieldToggles fieldToggles;
        public final String error;

        private HeartbeatResult(
            boolean ok,
            boolean unauthorized,
            String tokenExpiresAt,
            String rotatedToken,
            boolean guildOptIn,
            GatewayModels.FieldToggles fieldToggles,
            String error
        ) {
            this.ok = ok;
            this.unauthorized = unauthorized;
            this.tokenExpiresAt = tokenExpiresAt;
            this.rotatedToken = rotatedToken;
            this.guildOptIn = guildOptIn;
            this.fieldToggles = fieldToggles;
            this.error = error;
        }

        public static HeartbeatResult failed() {
            return failed("heartbeat_failed");
        }

        public static HeartbeatResult failed(String error) {
            return new HeartbeatResult(false, false, null, null, false, null, normalizeError(error));
        }
    }

    public static final class SubmitResult {
        public final boolean ok;
        public final boolean unauthorized;
        public final boolean terminal;
        public final int accepted;
        public final int rejected;
        public final String error;

        private SubmitResult(
            boolean ok,
            boolean unauthorized,
            boolean terminal,
            int accepted,
            int rejected,
            String error
        ) {
            this.ok = ok;
            this.unauthorized = unauthorized;
            this.terminal = terminal;
            this.accepted = Math.max(0, accepted);
            this.rejected = Math.max(0, rejected);
            this.error = error;
        }

        public static SubmitResult ok(int accepted, int rejected) {
            return new SubmitResult(true, false, true, accepted, rejected, null);
        }

        public static SubmitResult terminalRejected(String error, int accepted, int rejected) {
            return new SubmitResult(false, false, true, accepted, rejected, normalizeError(error));
        }

        public static SubmitResult failed() {
            return failed("upload_failed");
        }

        public static SubmitResult failed(String error) {
            return new SubmitResult(false, false, false, 0, 0, normalizeError(error));
        }
    }

    private EnrollResult enroll(
        String ingestBaseUrl,
        boolean allowInsecureIngestHttp,
        String reporterId,
        boolean guildOptIn,
        GatewayModels.FieldToggles fieldToggles,
        String minecraftVersion,
        String modVersion,
        String validityState,
        ReporterConfig config
    ) {
        ValidatedIngestBaseUrl validated = validateIngestBaseUrl(ingestBaseUrl, allowInsecureIngestHttp);
        if (!validated.ok()) {
            IrisReporterClient.LOGGER.warn("Enrollment blocked: {}", validated.error());
            return EnrollResult.failed(validated.error());
        }
        if (!ReporterSecurity.ensureDeviceIdentity(config)) {
            return EnrollResult.failed("device_identity_init_failed");
        }

        ReporterSecurity.SessionProof sessionProof = ReporterSecurity.captureSessionProof();
        if (!sessionProof.valid()) {
            IrisReporterClient.LOGGER.warn(
                "Session proof incomplete during enroll (uuid_present={}, username_present={}, token_present={})",
                sessionProof.mojangUuid != null && !sessionProof.mojangUuid.isBlank(),
                sessionProof.mojangUsername != null && !sessionProof.mojangUsername.isBlank(),
                sessionProof.sessionToken != null && !sessionProof.sessionToken.isBlank()
            );
        }
        GatewayModels.WorldAttestation worldAttestation = ReporterSecurity.buildWorldAttestation(validityState);

        GatewayModels.AttestChallengeRequest challengeRequest = new GatewayModels.AttestChallengeRequest();
        challengeRequest.device_pubkey = config.devicePublicKeyB64;
        challengeRequest.minecraft_version = minecraftVersion;
        challengeRequest.mod_version = modVersion;

        GatewayModels.AttestChallengeResponse challengeResponse = requestChallenge(validated.baseUrl(), challengeRequest);
        if (challengeResponse == null || !challengeResponse.ok || challengeResponse.challenge_id == null || challengeResponse.nonce == null || challengeResponse.server_id == null) {
            return EnrollResult.failed("attest_challenge_failed");
        }

        String devicePubkeyHash = ReporterSecurity.sha256Hex(config.devicePublicKeyB64 == null ? "" : config.devicePublicKeyB64);
        String enrollMessage = "enroll\n"
            + challengeResponse.challenge_id + "\n"
            + challengeResponse.nonce + "\n"
            + challengeResponse.server_id + "\n"
            + sessionProof.mojangUuid + "\n"
            + sessionProof.mojangUsername + "\n"
            + devicePubkeyHash + "\n"
            + (worldAttestation.observed_at == null ? "" : worldAttestation.observed_at);
        String deviceSig = ReporterSecurity.sign(config, enrollMessage);
        if (deviceSig == null || deviceSig.isBlank()) {
            return EnrollResult.failed("enroll_signature_failed");
        }

        GatewayModels.EnrollRequest requestBody = new GatewayModels.EnrollRequest();
        requestBody.reporter_id = reporterId;
        requestBody.guild_opt_in = guildOptIn;
        requestBody.minecraft_version = minecraftVersion;
        requestBody.mod_version = modVersion;
        requestBody.field_toggles = fieldToggles;
        requestBody.challenge_id = challengeResponse.challenge_id;
        requestBody.device_pubkey = config.devicePublicKeyB64;
        requestBody.device_sig = deviceSig;
        requestBody.mojang_uuid = sessionProof.mojangUuid;
        requestBody.mojang_username = sessionProof.mojangUsername;
        requestBody.server_id = challengeResponse.server_id;
        requestBody.world_attestation = worldAttestation;
        requestBody.session_token = sessionProof.sessionToken;

        HttpRequest request = HttpRequest.newBuilder()
            .uri(endpointUri(validated.baseUrl(), "/v1/enroll"))
            .header("Content-Type", "application/json")
            .timeout(Duration.ofSeconds(8))
            .POST(HttpRequest.BodyPublishers.ofString(GSON.toJson(requestBody)))
            .build();

        try {
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            if (response.statusCode() / 100 != 2) {
                String reason = "gateway_http_" + response.statusCode();
                IrisReporterClient.LOGGER.warn("Enrollment failed with status {}", response.statusCode());
                return EnrollResult.failed(reason);
            }

            GatewayModels.EnrollResponse enrollResponse = GSON.fromJson(response.body(), GatewayModels.EnrollResponse.class);
            if (enrollResponse == null || !enrollResponse.ok) {
                IrisReporterClient.LOGGER.warn("Enrollment response malformed: {}", response.body());
                return EnrollResult.failed("enroll_response_malformed");
            }

            return new EnrollResult(
                true,
                enrollResponse.reporter_id,
                enrollResponse.token,
                enrollResponse.token_expires_at,
                enrollResponse.guild_opt_in,
                enrollResponse.field_toggles,
                null
            );
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            IrisReporterClient.LOGGER.warn("Enrollment request failed", e);
            return EnrollResult.failed("enroll_request_failed");
        }
    }

    private HeartbeatResult heartbeat(
        String ingestBaseUrl,
        boolean allowInsecureIngestHttp,
        String token,
        boolean guildOptIn,
        GatewayModels.FieldToggles fieldToggles,
        String validityState,
        ReporterConfig config
    ) {
        ValidatedIngestBaseUrl validated = validateIngestBaseUrl(ingestBaseUrl, allowInsecureIngestHttp);
        if (!validated.ok()) {
            IrisReporterClient.LOGGER.warn("Heartbeat blocked: {}", validated.error());
            return HeartbeatResult.failed(validated.error());
        }
        if (!ReporterSecurity.ensureDeviceIdentity(config)) {
            return HeartbeatResult.failed("device_identity_init_failed");
        }

        GatewayModels.HeartbeatRequest heartbeatRequest = new GatewayModels.HeartbeatRequest();
        heartbeatRequest.guild_opt_in = guildOptIn;
        heartbeatRequest.field_toggles = fieldToggles;
        heartbeatRequest.world_attestation = ReporterSecurity.buildWorldAttestation(validityState);
        ReporterSecurity.SessionProof sessionProof = ReporterSecurity.captureSessionProof();
        heartbeatRequest.session_refresh_token = sessionProof.sessionToken;

        String payload = GSON.toJson(heartbeatRequest);
        String ts = Long.toString(System.currentTimeMillis() / 1000L);
        String nonce = UUID.randomUUID().toString();
        String keyId = config.deviceKeyId == null ? "" : config.deviceKeyId;
        String reporterId = config.reporterId == null ? "" : config.reporterId;
        String signedMessage = ReporterSecurity.canonicalSignedMessage(
            "POST",
            "/v1/heartbeat",
            ts,
            nonce,
            payload,
            reporterId
        );
        String signature = ReporterSecurity.sign(config, signedMessage);
        if (signature == null || signature.isBlank()) {
            return HeartbeatResult.failed("heartbeat_signature_failed");
        }

        HttpRequest request = HttpRequest.newBuilder()
            .uri(endpointUri(validated.baseUrl(), "/v1/heartbeat"))
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer " + token)
            .header("X-Iris-Key-Id", keyId)
            .header("X-Iris-Ts", ts)
            .header("X-Iris-Nonce", nonce)
            .header("X-Iris-Sig", signature)
            .timeout(Duration.ofSeconds(8))
            .POST(HttpRequest.BodyPublishers.ofString(payload))
            .build();

        try {
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            if (response.statusCode() / 100 != 2) {
                boolean unauthorized = response.statusCode() == 401;
                String reason = "gateway_http_" + response.statusCode();
                IrisReporterClient.LOGGER.debug("Heartbeat rejected: {}", response.statusCode());
                return new HeartbeatResult(false, unauthorized, null, null, guildOptIn, null, reason);
            }

            GatewayModels.HeartbeatResponse heartbeatResponse = GSON.fromJson(response.body(), GatewayModels.HeartbeatResponse.class);
            if (heartbeatResponse == null || !heartbeatResponse.ok) {
                return HeartbeatResult.failed("heartbeat_response_malformed");
            }

            return new HeartbeatResult(
                true,
                false,
                heartbeatResponse.token_expires_at,
                heartbeatResponse.rotated_token,
                heartbeatResponse.guild_opt_in,
                heartbeatResponse.field_toggles,
                null
            );
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            IrisReporterClient.LOGGER.debug("Heartbeat failed", e);
            return HeartbeatResult.failed("heartbeat_request_failed");
        }
    }

    private GatewayModels.AttestChallengeResponse requestChallenge(
        String baseUrl,
        GatewayModels.AttestChallengeRequest requestBody
    ) {
        HttpRequest request = HttpRequest.newBuilder()
            .uri(endpointUri(baseUrl, "/v1/attest/challenge"))
            .header("Content-Type", "application/json")
            .timeout(Duration.ofSeconds(8))
            .POST(HttpRequest.BodyPublishers.ofString(GSON.toJson(requestBody)))
            .build();
        try {
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            if (response.statusCode() / 100 != 2) {
                IrisReporterClient.LOGGER.debug("Attestation challenge rejected: {}", response.statusCode());
                return null;
            }
            GatewayModels.AttestChallengeResponse parsed = GSON.fromJson(response.body(), GatewayModels.AttestChallengeResponse.class);
            if (parsed == null || !parsed.ok) {
                return null;
            }
            return parsed;
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            IrisReporterClient.LOGGER.debug("Attestation challenge request failed", e);
            return null;
        }
    }

    private SubmitResult postAuthed(
        String ingestBaseUrl,
        boolean allowInsecureIngestHttp,
        String token,
        String path,
        Object body,
        String validityState,
        ReporterConfig config
    ) {
        ValidatedIngestBaseUrl validated = validateIngestBaseUrl(ingestBaseUrl, allowInsecureIngestHttp);
        if (!validated.ok()) {
            IrisReporterClient.LOGGER.warn("Upload blocked: {}", validated.error());
            return SubmitResult.failed(validated.error());
        }
        if (!ReporterSecurity.ensureDeviceIdentity(config)) {
            return SubmitResult.failed("device_identity_init_failed");
        }

        GatewayModels.WorldAttestation attestation = ReporterSecurity.buildWorldAttestation(validityState);
        ReporterSecurity.SessionProof sessionProof = ReporterSecurity.captureSessionProof();
        if (body instanceof GatewayModels.TerritoryBatch batch) {
            batch.world_attestation = attestation;
            batch.session_refresh_token = sessionProof.sessionToken;
        }
        String payload = GSON.toJson(body);
        String ts = Long.toString(System.currentTimeMillis() / 1000L);
        String nonce = UUID.randomUUID().toString();
        String keyId = config.deviceKeyId == null ? "" : config.deviceKeyId;
        String reporterId = config.reporterId == null ? "" : config.reporterId;
        String signedMessage = ReporterSecurity.canonicalSignedMessage(
            "POST",
            path,
            ts,
            nonce,
            payload,
            reporterId
        );
        String signature = ReporterSecurity.sign(config, signedMessage);
        if (signature == null || signature.isBlank()) {
            return SubmitResult.failed("upload_signature_failed");
        }

        HttpRequest request = HttpRequest.newBuilder()
            .uri(endpointUri(validated.baseUrl(), path))
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer " + token)
            .header("X-Iris-Key-Id", keyId)
            .header("X-Iris-Ts", ts)
            .header("X-Iris-Nonce", nonce)
            .header("X-Iris-Sig", signature)
            .timeout(Duration.ofSeconds(10))
            .POST(HttpRequest.BodyPublishers.ofString(payload))
            .build();

        try {
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            return interpretSubmitResponse(response.statusCode(), response.body());
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            IrisReporterClient.LOGGER.debug("Request failed for {}", path, e);
            return SubmitResult.failed("upload_request_failed");
        }
    }

    static SubmitResult interpretSubmitResponse(int statusCode, String responseBody) {
        boolean unauthorized = statusCode == 401;
        if (statusCode == 413) {
            // Payload too large is not recoverable by retrying the same queued batch.
            return SubmitResult.terminalRejected("gateway_http_413", 0, 0);
        }
        if (statusCode / 100 != 2) {
            return new SubmitResult(false, unauthorized, false, 0, 0, "gateway_http_" + statusCode);
        }

        SubmitAck ack = parseSubmitAck(responseBody);
        if (ack == null) {
            return SubmitResult.ok(0, 0);
        }
        if (!ack.ok()) {
            return SubmitResult.failed("upload_ack_not_ok");
        }
        if (ack.accepted() == 0 && ack.rejected() > 0) {
            return SubmitResult.terminalRejected("upload_rejected_all", 0, ack.rejected());
        }
        return SubmitResult.ok(ack.accepted(), ack.rejected());
    }

    private static SubmitAck parseSubmitAck(String responseBody) {
        if (responseBody == null || responseBody.isBlank()) {
            return null;
        }

        JsonObject object;
        try {
            object = GSON.fromJson(responseBody, JsonObject.class);
        } catch (RuntimeException e) {
            return null;
        }
        if (object == null) {
            return null;
        }

        boolean hasSubmitFields = object.has("ok")
            || object.has("accepted")
            || object.has("applied")
            || object.has("rejected");
        if (!hasSubmitFields) {
            return null;
        }

        boolean ok = readBoolean(object.get("ok"), true);
        int accepted = readInt(object.get("accepted"), Integer.MIN_VALUE);
        if (accepted == Integer.MIN_VALUE) {
            accepted = readInt(object.get("applied"), 0);
        }
        int rejected = readInt(object.get("rejected"), 0);
        return new SubmitAck(ok, accepted, rejected);
    }

    private static boolean readBoolean(JsonElement value, boolean fallback) {
        if (value == null || value.isJsonNull()) {
            return fallback;
        }
        try {
            return value.getAsBoolean();
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static int readInt(JsonElement value, int fallback) {
        if (value == null || value.isJsonNull()) {
            return fallback;
        }
        try {
            int parsed = value.getAsInt();
            return Math.max(0, parsed);
        } catch (RuntimeException ignored) {
            return fallback;
        }
    }

    private static String normalizeError(String error) {
        if (error == null || error.isBlank()) {
            return "unknown";
        }
        return error;
    }

    private static URI endpointUri(String baseUrl, String path) {
        return URI.create(baseUrl + path);
    }

    private static ValidatedIngestBaseUrl validateIngestBaseUrl(
        String ingestBaseUrl,
        boolean allowInsecureIngestHttp
    ) {
        if (ingestBaseUrl == null || ingestBaseUrl.isBlank()) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_missing");
        }

        String trimmed = ingestBaseUrl.trim();
        URI uri;
        try {
            uri = URI.create(trimmed);
        } catch (IllegalArgumentException e) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_invalid");
        }

        String scheme = uri.getScheme();
        if (scheme == null || scheme.isBlank()) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_missing_scheme");
        }

        String normalizedScheme = scheme.toLowerCase(Locale.ROOT);
        if (!normalizedScheme.equals("http") && !normalizedScheme.equals("https")) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_unsupported_scheme");
        }

        String host = uri.getHost();
        if ((host == null || host.isBlank()) && uri.getAuthority() != null) {
            host = extractHostFromAuthority(uri.getAuthority());
        }
        if (host == null || host.isBlank()) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_missing_host");
        }

        if (normalizedScheme.equals("http") && !allowInsecureIngestHttp && !isLoopbackHost(host)) {
            return ValidatedIngestBaseUrl.err("insecure_http_ingest_url_blocked");
        }

        String normalizedBase = trimmed;
        while (normalizedBase.endsWith("/")) {
            normalizedBase = normalizedBase.substring(0, normalizedBase.length() - 1);
        }
        if (normalizedBase.isEmpty()) {
            return ValidatedIngestBaseUrl.err("ingest_base_url_invalid");
        }

        return ValidatedIngestBaseUrl.ok(normalizedBase);
    }

    static String ingestUrlValidationError(String ingestBaseUrl, boolean allowInsecureIngestHttp) {
        return validateIngestBaseUrl(ingestBaseUrl, allowInsecureIngestHttp).error();
    }

    private static boolean isLoopbackHost(String host) {
        String normalized = host.trim().toLowerCase(Locale.ROOT);
        if (normalized.equals("localhost")
            || normalized.equals("::1")
            || normalized.equals("0:0:0:0:0:0:0:1")) {
            return true;
        }
        return isLoopbackIpv4Literal(normalized);
    }

    private static boolean isLoopbackIpv4Literal(String host) {
        String[] parts = host.split("\\.");
        if (parts.length != 4) {
            return false;
        }

        int firstOctet = -1;
        for (int idx = 0; idx < parts.length; idx++) {
            String part = parts[idx];
            if (part.isEmpty() || part.length() > 3) {
                return false;
            }
            for (int charIdx = 0; charIdx < part.length(); charIdx++) {
                if (!Character.isDigit(part.charAt(charIdx))) {
                    return false;
                }
            }
            int octet;
            try {
                octet = Integer.parseInt(part);
            } catch (NumberFormatException e) {
                return false;
            }
            if (octet < 0 || octet > 255) {
                return false;
            }
            if (idx == 0) {
                firstOctet = octet;
            }
        }
        return firstOctet == 127;
    }

    private static String extractHostFromAuthority(String authority) {
        String normalized = authority.trim();
        if (normalized.isEmpty()) {
            return null;
        }

        if (normalized.startsWith("[") && normalized.contains("]")) {
            int end = normalized.indexOf(']');
            return normalized.substring(1, end);
        }

        int separator = normalized.lastIndexOf(':');
        if (separator <= 0) {
            return normalized;
        }
        return normalized.substring(0, separator);
    }

    public static void invalidateToken(ReporterConfig config) {
        config.token = null;
        config.tokenExpiresAt = null;
        ConfigStore.save(config);
    }

    private record ValidatedIngestBaseUrl(String baseUrl, String error) {
        private static ValidatedIngestBaseUrl ok(String baseUrl) {
            return new ValidatedIngestBaseUrl(baseUrl, null);
        }

        private static ValidatedIngestBaseUrl err(String error) {
            return new ValidatedIngestBaseUrl(null, error);
        }

        private boolean ok() {
            return error == null;
        }
    }

    private record SubmitAck(boolean ok, int accepted, int rejected) {}
}
