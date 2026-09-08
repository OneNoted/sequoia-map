package io.iris.reporter.updater;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

import java.nio.file.Path;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

class UpdateJobTest {
    @TempDir
    Path directory;

    @Test
    void jobRoundTripsPathsAsJsonStrings() {
        UpdateJob job = new UpdateJob("job-1", 123, 1000, 2000,
            directory.resolve("current mod.jar"), directory.resolve("staged.jar"),
            directory.resolve("backup.jar"), directory.resolve("status.json"), "abc123", "1.2.3");

        String json = job.toJson();
        assertEquals(job, UpdateJob.fromJson(json));
        assertTrue(com.google.gson.JsonParser.parseString(json).getAsJsonObject()
            .get("targetJar").getAsJsonPrimitive().isString());
    }

    @Test
    void statusReadsHelperOutput() {
        assertTrue(UpdateJob.statusFromJson("{\"state\":\"success\",\"reason\":\"installed\"}").success());
        assertTrue(UpdateJob.statusFromJson("{\"state\":\"failed\",\"reason\":\"timeout\"}").failed());
    }
}
