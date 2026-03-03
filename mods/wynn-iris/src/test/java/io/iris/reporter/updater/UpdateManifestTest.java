package io.iris.reporter.updater;

import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

public class UpdateManifestTest {
    @Test
    void parsesSchemaV1AndFindsAssets() {
        String json = """
            {
              \"schema\": \"iris-update-manifest/v1\",
              \"release_tag\": \"iris-v0.2.0\",
              \"repo\": \"OneNoted/sequoia-map\",
              \"created_at\": \"2026-03-03T00:00:00Z\",
              \"assets\": [
                {
                  \"name\": \"wynn-iris-mc1.21.11-0.2.0.jar\",
                  \"type\": \"mod\",
                  \"minecraft\": \"1.21.11\",
                  \"version\": \"0.2.0\",
                  \"sha256\": \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",
                  \"size\": 123
                }
              ]
            }
            """;

        UpdateManifest manifest = UpdateManifest.parse(json.getBytes(StandardCharsets.UTF_8));
        assertEquals(UpdateManifest.SCHEMA, manifest.schema());
        assertEquals("iris-v0.2.0", manifest.releaseTag());

        UpdateManifest.Asset asset = manifest.findAssetByName("wynn-iris-mc1.21.11-0.2.0.jar");
        assertNotNull(asset);
        assertEquals("0.2.0", asset.version());
        assertEquals("1.21.11", asset.minecraft());
        assertEquals(123L, asset.size());
        assertNull(manifest.findAssetByName("missing.jar"));
    }

    @Test
    void rejectsUnsupportedSchema() {
        String json = """
            {
              \"schema\": \"wrong\",
              \"assets\": [
                { \"name\": \"a.jar\", \"sha256\": \"00\" }
              ]
            }
            """;

        assertThrows(IllegalArgumentException.class, () -> UpdateManifest.parse(json.getBytes(StandardCharsets.UTF_8)));
    }
}
