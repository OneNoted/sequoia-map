package io.iris.reporter.updater;

import java.nio.file.Path;

public record UpdateStage(
    Path targetJar,
    Path stagedJar,
    String expectedSha256,
    String releaseVersion,
    String releaseUrl,
    String assetUrl
) {}
