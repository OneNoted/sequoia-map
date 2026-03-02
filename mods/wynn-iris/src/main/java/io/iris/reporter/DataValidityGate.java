package io.iris.reporter;

import java.util.Locale;
import java.util.Objects;

/**
 * Tracks whether territory data is currently safe to collect and submit.
 *
 * <p>State transitions are driven by packet/world signals and stabilized with
 * movement + time checks before resuming from an AFK/invalid pause.
 */
public final class DataValidityGate {
    public enum State {
        VALID("valid"),
        PAUSED_AFK("paused_afk"),
        PAUSED_INVALID_WORLD("paused_invalid_world"),
        RECOVERING("recovering");

        private final String id;

        State(String id) {
            this.id = id;
        }

        public String id() {
            return id;
        }
    }

    private static final float ROTATION_DELTA_THRESHOLD_DEGREES = 8.0f;
    private static final String REASON_NONE = "none";

    private final long resumeStabilizationMs;
    private final double minMovementBlocksSquared;

    private State state = State.VALID;
    private String pauseReason = REASON_NONE;
    private long stateSinceMs;

    private long recoveryCandidateSinceMs;
    private boolean movementSeenDuringRecovery;

    private boolean hasLastPose;
    private double lastX;
    private double lastY;
    private double lastZ;
    private float lastYaw;
    private float lastPitch;

    public DataValidityGate(long resumeStabilizationMs, double minMovementBlocks) {
        this.resumeStabilizationMs = Math.max(1_000L, resumeStabilizationMs);
        double movement = Math.max(0.05, minMovementBlocks);
        this.minMovementBlocksSquared = movement * movement;
        this.stateSinceMs = System.currentTimeMillis();
    }

    public synchronized State state() {
        return state;
    }

    public synchronized String stateId() {
        return state.id();
    }

    public synchronized String pauseReason() {
        return pauseReason;
    }

    public synchronized long stateSinceMs() {
        return stateSinceMs;
    }

    public synchronized boolean allowCollection() {
        return state == State.VALID;
    }

    public synchronized boolean allowDispatch() {
        return state == State.VALID;
    }

    public synchronized boolean movementSeenDuringRecovery() {
        return movementSeenDuringRecovery;
    }

    public synchronized long recoveryRemainingMs(long nowMs) {
        if (state != State.RECOVERING || recoveryCandidateSinceMs <= 0L) {
            return 0L;
        }
        long elapsed = Math.max(0L, nowMs - recoveryCandidateSinceMs);
        return Math.max(0L, resumeStabilizationMs - elapsed);
    }

    public synchronized void onTitleText(String rawText, long nowMs) {
        String text = normalizeText(rawText);
        if (text.contains("YOU ARE AFK") || text.contains("MOVE TO CONTINUE")) {
            transitionTo(State.PAUSED_AFK, "title_afk", nowMs);
        }
    }

    public synchronized void onSubtitleText(String rawText, long nowMs) {
        String text = normalizeText(rawText);
        if (text.contains("YOU ARE AFK") || text.contains("MOVE TO CONTINUE")) {
            transitionTo(State.PAUSED_AFK, "subtitle_afk", nowMs);
        }
    }

    public synchronized void onTitleClear(long nowMs) {
        if (state == State.PAUSED_AFK) {
            pauseReason = "title_cleared_waiting_world";
            stateSinceMs = nowMs;
        }
    }

    public synchronized void onWorldSignal(String packetType, String details, long nowMs) {
        String packet = normalizeFreeform(packetType);
        String full = normalizeFreeform(details);

        if (full.contains("WYNNCRAFT:CRUNKLE")) {
            transitionTo(State.PAUSED_INVALID_WORLD, "world_crunkle", nowMs);
            return;
        }
        if (full.contains("MINECRAFT:THE_VOID") || full.contains("THE_VOID")) {
            transitionTo(State.PAUSED_INVALID_WORLD, "world_void", nowMs);
            return;
        }

        boolean hasOverworld = full.contains("MINECRAFT:OVERWORLD");
        boolean joinOrRespawn = packet.contains("GAMEJOIN") || packet.contains("RESPAWN");
        if (hasOverworld && joinOrRespawn) {
            transitionToRecovering("world_overworld", nowMs);
        }
    }

    public synchronized void onTickPose(
        long nowMs,
        boolean hasPlayer,
        double x,
        double y,
        double z,
        float yaw,
        float pitch
    ) {
        if (!hasPlayer) {
            hasLastPose = false;
            return;
        }

        boolean moved = false;
        if (hasLastPose) {
            double dx = x - lastX;
            double dy = y - lastY;
            double dz = z - lastZ;
            double distSq = (dx * dx) + (dy * dy) + (dz * dz);
            float yawDelta = angleDelta(lastYaw, yaw);
            float pitchDelta = angleDelta(lastPitch, pitch);
            moved = distSq >= minMovementBlocksSquared
                || yawDelta >= ROTATION_DELTA_THRESHOLD_DEGREES
                || pitchDelta >= ROTATION_DELTA_THRESHOLD_DEGREES;
        }

        lastX = x;
        lastY = y;
        lastZ = z;
        lastYaw = yaw;
        lastPitch = pitch;
        hasLastPose = true;

        if (state == State.PAUSED_AFK && moved) {
            transitionToRecovering("movement_resume", nowMs);
            movementSeenDuringRecovery = true;
            return;
        }

        if (state != State.RECOVERING) {
            return;
        }

        if (moved) {
            movementSeenDuringRecovery = true;
        }

        long stabilizedFor = Math.max(0L, nowMs - recoveryCandidateSinceMs);
        if (movementSeenDuringRecovery && stabilizedFor >= resumeStabilizationMs) {
            transitionTo(State.VALID, REASON_NONE, nowMs);
            recoveryCandidateSinceMs = 0L;
            movementSeenDuringRecovery = false;
        }
    }

    private void transitionToRecovering(String reason, long nowMs) {
        if (state == State.VALID) {
            return;
        }
        state = State.RECOVERING;
        pauseReason = reason;
        stateSinceMs = nowMs;
        recoveryCandidateSinceMs = nowMs;
        movementSeenDuringRecovery = false;
    }

    private void transitionTo(State nextState, String reason, long nowMs) {
        if (state == nextState && Objects.equals(pauseReason, reason)) {
            return;
        }
        state = nextState;
        pauseReason = reason;
        stateSinceMs = nowMs;
        if (nextState != State.RECOVERING) {
            recoveryCandidateSinceMs = 0L;
            movementSeenDuringRecovery = false;
        }
    }

    private static float angleDelta(float from, float to) {
        float delta = Math.abs(from - to) % 360.0f;
        return delta > 180.0f ? 360.0f - delta : delta;
    }

    private static String normalizeText(String value) {
        if (value == null || value.isBlank()) {
            return "";
        }
        return value
            .toUpperCase(Locale.ROOT)
            .replaceAll("§[0-9A-FK-ORX]", " ")
            .replaceAll("[^A-Z0-9 ]", " ")
            .replaceAll("\\s+", " ")
            .trim();
    }

    private static String normalizeFreeform(String value) {
        if (value == null || value.isBlank()) {
            return "";
        }
        return value
            .toUpperCase(Locale.ROOT)
            .replaceAll("§[0-9A-FK-ORX]", " ")
            .replaceAll("\\s+", " ")
            .trim();
    }
}
