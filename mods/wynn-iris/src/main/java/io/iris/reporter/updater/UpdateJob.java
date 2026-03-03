package io.iris.reporter.updater;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;

import java.nio.file.Path;
import java.util.Objects;

public record UpdateJob(
    String id,
    long parentPid,
    long createdAtEpochMs,
    long deadlineEpochMs,
    Path targetJar,
    Path stagedJar,
    Path backupJar,
    Path statusPath,
    String expectedSha256,
    String targetVersion
) {
    private static final Gson GSON = new GsonBuilder().create();

    public String toJson() {
        return GSON.toJson(this);
    }

    public static UpdateJob fromJson(String json) {
        return Objects.requireNonNull(GSON.fromJson(json, UpdateJob.class), "job");
    }

    public record Status(String state, String reason, String finishedAt) {
        public boolean success() {
            return "success".equalsIgnoreCase(state);
        }

        public boolean failed() {
            return "failed".equalsIgnoreCase(state);
        }
    }

    public static Status statusFromJson(String json) {
        return Objects.requireNonNull(GSON.fromJson(json, Status.class), "status");
    }

    public static String statusToJson(Status status) {
        return GSON.toJson(status);
    }
}
