package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

import static org.junit.jupiter.api.Assertions.assertTrue;

public class MixinConfigWiringTest {
    @Test
    void fabricModDeclaresReporterMixinConfig() throws IOException {
        String json = readResource("fabric.mod.json");
        assertTrue(json.contains("\"wynn-iris.mixins.json\""));
        assertTrue(json.contains("\"wynn_iris\""));
        assertTrue(json.contains("\"io.iris.reporter.IrisReporterClient\""));
    }

    @Test
    void mixinConfigDeclaresNetworkHandlerMixin() throws IOException {
        String json = readResource("wynn-iris.mixins.json");
        assertTrue(json.contains("\"required\": true"));
        assertTrue(json.contains("\"ClientPlayNetworkHandlerMixin\""));
        assertTrue(json.contains("\"defaultRequire\": 1"));
    }

    private static String readResource(String name) throws IOException {
        try (InputStream stream = MixinConfigWiringTest.class.getClassLoader().getResourceAsStream(name)) {
            if (stream == null) {
                throw new IOException("missing resource: " + name);
            }
            return new String(stream.readAllBytes(), StandardCharsets.UTF_8);
        }
    }
}
