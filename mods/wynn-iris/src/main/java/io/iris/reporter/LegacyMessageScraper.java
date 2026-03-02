package io.iris.reporter;

import java.time.Instant;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public final class LegacyMessageScraper {
    private static final Pattern TERRITORY_CAPTURE_PATTERN = Pattern.compile(
        "(?<territory>[A-Za-z'\\- ]+)\\s+was\\s+captured\\s+by\\s+(?<prefix>\\[[A-Za-z0-9]{2,6}])",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern WAR_QUEUED_PATTERN = Pattern.compile(
        "(?<territory>[A-Za-z'\\- ]+)\\s+is\\s+under\\s+attack",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern WAR_STARTED_PATTERN = Pattern.compile(
        "War\\s+started\\s+at\\s+(?<territory>[A-Za-z'\\- ]+)",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern WAR_CAPTURED_PATTERN = Pattern.compile(
        "(?<territory>[A-Za-z'\\- ]+)\\s+was\\s+captured",
        Pattern.CASE_INSENSITIVE
    );

    private LegacyMessageScraper() {}

    public record CaptureSignal(
        String territory,
        String guildPrefix,
        String observedAt,
        long observedAtMs,
        String rawMessage
    ) {
    }

    public record WarSignal(
        String territory,
        String kind,
        String observedAt,
        long observedAtMs,
        String rawMessage
    ) {
    }

    public static CaptureSignal parseCapture(String rawMessage, long observedAtMs) {
        if (rawMessage == null || rawMessage.isBlank()) {
            return null;
        }

        Matcher capture = TERRITORY_CAPTURE_PATTERN.matcher(rawMessage);
        if (!capture.find()) {
            return null;
        }

        String territory = normalize(capture.group("territory"));
        String prefix = normalize(capture.group("prefix"));
        if (territory.isEmpty() || prefix.isEmpty()) {
            return null;
        }

        return new CaptureSignal(
            territory,
            prefix.replace("[", "").replace("]", ""),
            Instant.ofEpochMilli(observedAtMs).toString(),
            observedAtMs,
            normalize(rawMessage)
        );
    }

    public static WarSignal parseWar(String rawMessage, long observedAtMs) {
        if (rawMessage == null || rawMessage.isBlank()) {
            return null;
        }

        Matcher queued = WAR_QUEUED_PATTERN.matcher(rawMessage);
        if (queued.find()) {
            return new WarSignal(
                normalize(queued.group("territory")),
                "queued",
                Instant.ofEpochMilli(observedAtMs).toString(),
                observedAtMs,
                normalize(rawMessage)
            );
        }

        Matcher started = WAR_STARTED_PATTERN.matcher(rawMessage);
        if (started.find()) {
            return new WarSignal(
                normalize(started.group("territory")),
                "started",
                Instant.ofEpochMilli(observedAtMs).toString(),
                observedAtMs,
                normalize(rawMessage)
            );
        }

        Matcher captured = WAR_CAPTURED_PATTERN.matcher(rawMessage);
        if (captured.find()) {
            return new WarSignal(
                normalize(captured.group("territory")),
                "captured",
                Instant.ofEpochMilli(observedAtMs).toString(),
                observedAtMs,
                normalize(rawMessage)
            );
        }

        return null;
    }

    private static String normalize(String value) {
        if (value == null) {
            return "";
        }
        return value.trim().replaceAll("\\s+", " ");
    }
}
