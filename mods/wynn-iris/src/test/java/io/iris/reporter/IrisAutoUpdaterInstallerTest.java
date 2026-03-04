package io.iris.reporter;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

public class IrisAutoUpdaterInstallerTest {
    @TempDir
    Path tempDir;

    @Test
    void installReplacesJarAndCreatesBackup() throws IOException {
        Path currentJar = tempDir.resolve("wynn-iris.jar");
        byte[] oldPayload = "old".getBytes(StandardCharsets.UTF_8);
        byte[] newPayload = "new".getBytes(StandardCharsets.UTF_8);
        Files.write(currentJar, oldPayload);

        IrisAutoUpdater.ApplyResult result = IrisAutoUpdater.installDownloadedJar(currentJar, newPayload);

        assertEquals(IrisAutoUpdater.ApplyStatus.APPLIED, result.status());
        assertArrayEquals(newPayload, Files.readAllBytes(currentJar));
        Path backup = tempDir.resolve("wynn-iris.jar.bak");
        assertArrayEquals(oldPayload, Files.readAllBytes(backup));
    }

    @Test
    void installRollsBackWhenReplaceFails() throws IOException {
        Path currentJar = tempDir.resolve("wynn-iris.jar");
        byte[] oldPayload = "old".getBytes(StandardCharsets.UTF_8);
        byte[] newPayload = "new".getBytes(StandardCharsets.UTF_8);
        Files.write(currentJar, oldPayload);

        IrisAutoUpdater.ApplyResult result = IrisAutoUpdater.installDownloadedJar(
            currentJar,
            newPayload,
            (source, target) -> {
                throw new IllegalStateException("simulated replace failure");
            }
        );

        assertEquals(IrisAutoUpdater.ApplyStatus.FAILED, result.status());
        assertEquals("update_install_failed", result.reason());
        assertArrayEquals(oldPayload, Files.readAllBytes(currentJar));
        Path backup = tempDir.resolve("wynn-iris.jar.bak");
        assertArrayEquals(oldPayload, Files.readAllBytes(backup));
    }

    @Test
    void rejectsUntrustedDownloadHost() {
        assertEquals(
            "update_asset_host_not_allowed",
            IrisAutoUpdater.downloadUrlValidationError("https://example.com/wynn-iris.jar")
        );
    }

    @Test
    void allowsTrustedGitHubDownloadHosts() {
        assertNull(IrisAutoUpdater.downloadUrlValidationError("https://github.com/OneNoted/sequoia-map/releases/download/iris-v0.1.1/wynn-iris.jar"));
        assertNull(IrisAutoUpdater.downloadUrlValidationError("https://release-assets.githubusercontent.com/wynn-iris.jar"));
        assertNull(IrisAutoUpdater.downloadUrlValidationError("https://objects.githubusercontent.com/wynn-iris.jar"));
        assertNull(IrisAutoUpdater.downloadUrlValidationError("https://github-releases.githubusercontent.com/wynn-iris.jar"));
    }

    @Test
    void resolveCurrentJarPathRejectsDevDirectoryLocation() {
        Path classesDir = tempDir.resolve("classes");
        try {
            Files.createDirectories(classesDir);
        } catch (IOException e) {
            throw new IllegalStateException(e);
        }

        assertNull(IrisAutoUpdater.resolveCurrentJarPathFromLocation(classesDir.toUri()));
    }
}
