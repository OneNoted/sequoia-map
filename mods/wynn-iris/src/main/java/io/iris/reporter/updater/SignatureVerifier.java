package io.iris.reporter.updater;

import java.security.GeneralSecurityException;
import java.security.KeyFactory;
import java.security.PublicKey;
import java.security.Signature;
import java.security.spec.X509EncodedKeySpec;
import java.util.Base64;
import java.util.Locale;
import java.util.Objects;

public final class SignatureVerifier {
    private final PublicKey publicKey;

    public SignatureVerifier(PublicKey publicKey) {
        this.publicKey = Objects.requireNonNull(publicKey, "publicKey");
    }

    public static SignatureVerifier fromBase64DerPublicKey(String base64Der) throws GeneralSecurityException {
        if (base64Der == null || base64Der.isBlank()) {
            throw new GeneralSecurityException("public_key_missing");
        }
        byte[] der = Base64.getDecoder().decode(base64Der.trim());
        KeyFactory keyFactory = KeyFactory.getInstance("Ed25519");
        PublicKey key = keyFactory.generatePublic(new X509EncodedKeySpec(der));
        return new SignatureVerifier(key);
    }

    public boolean verify(byte[] message, byte[] rawOrEncodedSignature) throws GeneralSecurityException {
        if (message == null || rawOrEncodedSignature == null || rawOrEncodedSignature.length == 0) {
            return false;
        }
        byte[] signatureBytes = decodeSignature(rawOrEncodedSignature);
        Signature signature = Signature.getInstance("Ed25519");
        signature.initVerify(publicKey);
        signature.update(message);
        return signature.verify(signatureBytes);
    }

    static byte[] decodeSignature(byte[] rawOrEncoded) {
        if (rawOrEncoded.length == 64) {
            return rawOrEncoded;
        }
        String text = new String(rawOrEncoded).trim();
        if (text.isEmpty()) {
            return rawOrEncoded;
        }

        if (looksHex(text)) {
            return decodeHex(text);
        }

        try {
            return Base64.getDecoder().decode(text);
        } catch (IllegalArgumentException ignored) {
            return rawOrEncoded;
        }
    }

    private static boolean looksHex(String value) {
        if ((value.length() & 1) != 0) {
            return false;
        }
        String normalized = value.toLowerCase(Locale.ROOT);
        for (int i = 0; i < normalized.length(); i++) {
            char c = normalized.charAt(i);
            boolean hex = (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
            if (!hex) {
                return false;
            }
        }
        return true;
    }

    private static byte[] decodeHex(String value) {
        int length = value.length();
        byte[] out = new byte[length / 2];
        for (int i = 0; i < length; i += 2) {
            int high = Character.digit(value.charAt(i), 16);
            int low = Character.digit(value.charAt(i + 1), 16);
            out[i / 2] = (byte) ((high << 4) + low);
        }
        return out;
    }
}
