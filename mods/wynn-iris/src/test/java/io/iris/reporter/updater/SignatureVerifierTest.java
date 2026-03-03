package io.iris.reporter.updater;

import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.Signature;
import java.util.HexFormat;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class SignatureVerifierTest {
    @Test
    void verifiesRawSignatureFromEd25519Keypair() throws Exception {
        KeyPairGenerator generator = KeyPairGenerator.getInstance("Ed25519");
        KeyPair keyPair = generator.generateKeyPair();

        byte[] payload = "hello-iris".getBytes(StandardCharsets.UTF_8);
        Signature signer = Signature.getInstance("Ed25519");
        signer.initSign(keyPair.getPrivate());
        signer.update(payload);
        byte[] signature = signer.sign();

        SignatureVerifier verifier = new SignatureVerifier(keyPair.getPublic());
        assertTrue(verifier.verify(payload, signature));
    }

    @Test
    void rejectsTamperedPayload() throws Exception {
        KeyPairGenerator generator = KeyPairGenerator.getInstance("Ed25519");
        KeyPair keyPair = generator.generateKeyPair();

        byte[] payload = "hello-iris".getBytes(StandardCharsets.UTF_8);
        Signature signer = Signature.getInstance("Ed25519");
        signer.initSign(keyPair.getPrivate());
        signer.update(payload);
        byte[] signature = signer.sign();

        SignatureVerifier verifier = new SignatureVerifier(keyPair.getPublic());
        assertFalse(verifier.verify("tampered".getBytes(StandardCharsets.UTF_8), signature));
    }

    @Test
    void decodesHexEncodedSignature() throws Exception {
        KeyPairGenerator generator = KeyPairGenerator.getInstance("Ed25519");
        KeyPair keyPair = generator.generateKeyPair();

        byte[] payload = "manifest".getBytes(StandardCharsets.UTF_8);
        Signature signer = Signature.getInstance("Ed25519");
        signer.initSign(keyPair.getPrivate());
        signer.update(payload);
        byte[] signature = signer.sign();

        String hex = HexFormat.of().formatHex(signature);
        SignatureVerifier verifier = new SignatureVerifier(keyPair.getPublic());
        assertTrue(verifier.verify(payload, hex.getBytes(StandardCharsets.UTF_8)));
    }
}
