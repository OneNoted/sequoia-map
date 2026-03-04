package io.iris.reporter;

import net.minecraft.client.MinecraftClient;

import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.MessageDigest;
import java.security.PrivateKey;
import java.security.Signature;
import java.security.spec.PKCS8EncodedKeySpec;
import java.time.Instant;
import java.util.Base64;
import java.util.Locale;

public final class ReporterSecurity {
    private ReporterSecurity() {}

    public static final class SessionProof {
        public final String mojangUuid;
        public final String mojangUsername;
        public final String sessionToken;

        SessionProof(String mojangUuid, String mojangUsername, String sessionToken) {
            this.mojangUuid = mojangUuid;
            this.mojangUsername = mojangUsername;
            this.sessionToken = sessionToken;
        }

        public boolean valid() {
            return mojangUuid != null && !mojangUuid.isBlank()
                && mojangUsername != null && !mojangUsername.isBlank()
                && sessionToken != null && !sessionToken.isBlank();
        }
    }

    public static boolean ensureDeviceIdentity(ReporterConfig config) {
        if (config == null) {
            return false;
        }
        if (config.devicePrivateKeyB64 != null && !config.devicePrivateKeyB64.isBlank()
            && config.devicePublicKeyB64 != null && !config.devicePublicKeyB64.isBlank()
            && config.deviceKeyId != null && !config.deviceKeyId.isBlank()) {
            return true;
        }

        try {
            KeyPairGenerator generator = KeyPairGenerator.getInstance("Ed25519");
            KeyPair keyPair = generator.generateKeyPair();
            String privateB64 = Base64.getEncoder().encodeToString(keyPair.getPrivate().getEncoded());
            String publicB64 = Base64.getEncoder().encodeToString(keyPair.getPublic().getEncoded());
            String keyId = keyId(publicB64);
            config.devicePrivateKeyB64 = privateB64;
            config.devicePublicKeyB64 = publicB64;
            config.deviceKeyId = keyId;
            return true;
        } catch (Exception e) {
            IrisReporterClient.LOGGER.warn("Failed to generate reporter device identity", e);
            return false;
        }
    }

    public static String sign(ReporterConfig config, String message) {
        if (config == null || config.devicePrivateKeyB64 == null || config.devicePrivateKeyB64.isBlank()) {
            return null;
        }
        if (message == null) {
            return null;
        }
        try {
            byte[] privateDer = Base64.getDecoder().decode(config.devicePrivateKeyB64.trim());
            PKCS8EncodedKeySpec spec = new PKCS8EncodedKeySpec(privateDer);
            KeyFactory keyFactory = KeyFactory.getInstance("Ed25519");
            PrivateKey privateKey = keyFactory.generatePrivate(spec);
            Signature signer = Signature.getInstance("Ed25519");
            signer.initSign(privateKey);
            signer.update(message.getBytes(StandardCharsets.UTF_8));
            return Base64.getEncoder().encodeToString(signer.sign());
        } catch (Exception e) {
            IrisReporterClient.LOGGER.warn("Failed to sign request", e);
            return null;
        }
    }

    public static GatewayModels.WorldAttestation buildWorldAttestation(String validityState) {
        GatewayModels.WorldAttestation attestation = new GatewayModels.WorldAttestation();
        attestation.server_host = currentServerHost();
        attestation.validity_state = validityState == null ? "unknown" : validityState;
        attestation.observed_at = Instant.now().toString();
        attestation.packet_hint = "client_runtime";
        return attestation;
    }

    public static SessionProof captureSessionProof() {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null) {
            return new SessionProof(null, null, null);
        }
        Object session = invoke(client, "getSession");
        if (session == null) {
            return new SessionProof(null, null, null);
        }
        String username = asString(invoke(session, "getUsername"));
        String sessionId = asString(invoke(session, "getSessionId"));
        String token = asString(invoke(session, "getAccessToken"));
        if (token == null || token.isBlank()) {
            token = asString(readField(session, "accessToken"));
        }
        token = normalizeSessionAccessToken(token, sessionId);

        String uuid = asString(invoke(session, "getUuidOrNull"));
        if (uuid == null || uuid.isBlank()) {
            uuid = asString(invoke(session, "getUuid"));
        }
        uuid = normalizeUuid(uuid);

        return new SessionProof(uuid, username, token);
    }

    public static String keyId(String publicKeyB64) {
        if (publicKeyB64 == null || publicKeyB64.isBlank()) {
            return "";
        }
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] hash = digest.digest(publicKeyB64.getBytes(StandardCharsets.UTF_8));
            return toHex(hash).substring(0, 16);
        } catch (Exception e) {
            return "unknown";
        }
    }

    public static String canonicalSignedMessage(String method, String path, String ts, String nonce, String bodyJson, String reporterId) {
        String bodyHash = sha256Hex(bodyJson == null ? "" : bodyJson);
        return method + "\n" + path + "\n" + ts + "\n" + nonce + "\n" + bodyHash + "\n" + reporterId;
    }

    public static String sha256Hex(String value) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            return toHex(digest.digest(value.getBytes(StandardCharsets.UTF_8)));
        } catch (Exception e) {
            return "";
        }
    }

    private static String toHex(byte[] bytes) {
        StringBuilder out = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            out.append(String.format(Locale.ROOT, "%02x", b));
        }
        return out.toString();
    }

    private static String currentServerHost() {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null) {
            return "";
        }
        Object serverEntry = invoke(client, "getCurrentServerEntry");
        if (serverEntry == null) {
            return "";
        }
        String address = asString(readField(serverEntry, "address"));
        if (address == null || address.isBlank()) {
            return "";
        }
        String trimmed = address.trim().toLowerCase(Locale.ROOT);
        int colon = trimmed.indexOf(':');
        if (colon > 0) {
            return trimmed.substring(0, colon);
        }
        return trimmed;
    }

    private static String normalizeUuid(String raw) {
        if (raw == null || raw.isBlank()) {
            return null;
        }
        return raw.trim().replace("-", "").toLowerCase(Locale.ROOT);
    }

    static String parseAccessTokenFromSessionId(String sessionId) {
        if (sessionId == null || sessionId.isBlank()) {
            return null;
        }
        String normalized = sessionId.trim();
        if (!normalized.startsWith("token:")) {
            return null;
        }
        String[] parts = normalized.split(":", 3);
        if (parts.length < 3) {
            return null;
        }
        String token = parts[1] == null ? "" : parts[1].trim();
        if (token.isBlank()) {
            return null;
        }
        return token;
    }

    static String normalizeSessionAccessToken(String token, String sessionId) {
        String normalized = token == null ? null : token.trim();
        String parsedFromSessionId = parseAccessTokenFromSessionId(sessionId);
        if (normalized == null || normalized.isBlank() || "0".equals(normalized) || "null".equalsIgnoreCase(normalized)) {
            return parsedFromSessionId;
        }
        String parsedFromToken = parseAccessTokenFromSessionId(normalized);
        if (parsedFromToken != null && !parsedFromToken.isBlank()) {
            return parsedFromToken;
        }
        return normalized;
    }

    private static String asString(Object value) {
        if (value == null) {
            return null;
        }
        String out = value.toString();
        return out == null || out.isBlank() ? null : out;
    }

    private static Object invoke(Object target, String methodName) {
        if (target == null || methodName == null) {
            return null;
        }
        try {
            Method method = target.getClass().getMethod(methodName);
            method.setAccessible(true);
            return method.invoke(target);
        } catch (Exception ignored) {
            return null;
        }
    }

    private static Object readField(Object target, String fieldName) {
        if (target == null || fieldName == null) {
            return null;
        }
        try {
            var field = target.getClass().getField(fieldName);
            field.setAccessible(true);
            return field.get(target);
        } catch (Exception ignored) {
            return null;
        }
    }
}
