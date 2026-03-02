package io.iris.reporter;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class DataValidityGateTest {
    @Test
    void subtitleAfkSignalPausesCollection() {
        DataValidityGate gate = new DataValidityGate(10_000L, 0.25);
        long now = 1_000L;
        gate.onSubtitleText("Move to continue", now);

        assertEquals(DataValidityGate.State.PAUSED_AFK, gate.state());
        assertFalse(gate.allowCollection());
    }

    @Test
    void crunkleWorldSignalPausesWithInvalidWorldState() {
        DataValidityGate gate = new DataValidityGate(10_000L, 0.25);
        long now = 1_000L;
        gate.onWorldSignal("PlayerRespawnS2CPacket", "dimension=wynncraft:crunkle", now);

        assertEquals(DataValidityGate.State.PAUSED_INVALID_WORLD, gate.state());
        assertFalse(gate.allowDispatch());
    }

    @Test
    void resumeRequiresOverworldAndMovementAndStabilityWindow() {
        DataValidityGate gate = new DataValidityGate(10_000L, 0.25);
        long start = 5_000L;

        gate.onSubtitleText("You are AFK", start);
        assertEquals(DataValidityGate.State.PAUSED_AFK, gate.state());

        gate.onWorldSignal(
            "GameJoinS2CPacket",
            "dimension=ResourceKey[minecraft:dimension / minecraft:overworld]",
            start + 500L
        );
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());
        assertFalse(gate.allowCollection());

        gate.onTickPose(start + 4_000L, true, 0.0, 64.0, 0.0, 0.0f, 0.0f);
        gate.onTickPose(start + 4_200L, true, 1.0, 64.0, 0.0, 0.0f, 0.0f);
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());

        gate.onTickPose(start + 10_000L, true, 1.0, 64.0, 0.0, 0.0f, 0.0f);
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());

        gate.onTickPose(start + 10_600L, true, 1.0, 64.0, 0.0, 0.0f, 0.0f);
        assertEquals(DataValidityGate.State.VALID, gate.state());
        assertTrue(gate.allowCollection());
    }

    @Test
    void afkSignalDuringRecoveryResetsToPaused() {
        DataValidityGate gate = new DataValidityGate(10_000L, 0.25);
        long start = 1_000L;
        gate.onSubtitleText("Move to continue", start);
        gate.onWorldSignal("GameJoinS2CPacket", "minecraft:overworld", start + 100L);
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());

        gate.onTitleText("YOU ARE AFK", start + 300L);
        assertEquals(DataValidityGate.State.PAUSED_AFK, gate.state());
    }

    @Test
    void movementCanResumeFromAfkWithoutWorldPacket() {
        DataValidityGate gate = new DataValidityGate(3_000L, 0.25);
        long start = 10_000L;
        gate.onSubtitleText("Move to continue", start);
        assertEquals(DataValidityGate.State.PAUSED_AFK, gate.state());

        // Establish baseline pose then move enough to clear AFK state.
        gate.onTickPose(start + 100L, true, 0.0, 64.0, 0.0, 0.0f, 0.0f);
        gate.onTickPose(start + 350L, true, 1.0, 64.0, 0.0, 0.0f, 0.0f);
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());

        // Once stabilization elapses, gate should return to VALID.
        gate.onTickPose(start + 3_450L, true, 1.0, 64.0, 0.0, 0.0f, 0.0f);
        assertEquals(DataValidityGate.State.VALID, gate.state());
        assertTrue(gate.allowDispatch());
    }

    @Test
    void titleClearUpdatesPauseReasonWhileAfk() {
        DataValidityGate gate = new DataValidityGate(10_000L, 0.25);
        long now = 50_000L;
        gate.onTitleText("YOU ARE AFK", now);
        gate.onTitleClear(now + 250L);

        assertEquals(DataValidityGate.State.PAUSED_AFK, gate.state());
        assertEquals("title_cleared_waiting_world", gate.pauseReason());
    }

    @Test
    void invalidWorldRecoveryReportsProgressAndRequiresMovement() {
        DataValidityGate gate = new DataValidityGate(2_000L, 0.25);
        long start = 20_000L;

        gate.onWorldSignal("PlayerRespawnS2CPacket", "wynncraft:crunkle", start);
        assertEquals(DataValidityGate.State.PAUSED_INVALID_WORLD, gate.state());

        gate.onWorldSignal("GameJoinS2CPacket", "minecraft:overworld", start + 100L);
        assertEquals(DataValidityGate.State.RECOVERING, gate.state());
        assertFalse(gate.movementSeenDuringRecovery());
        assertEquals(2_000L, gate.recoveryRemainingMs(start + 100L));

        gate.onTickPose(start + 1_200L, true, 0.0, 64.0, 0.0, 0.0f, 0.0f);
        gate.onTickPose(start + 1_300L, true, 0.0, 64.0, 0.0, 9.0f, 0.0f);
        assertTrue(gate.movementSeenDuringRecovery());
        assertTrue(gate.recoveryRemainingMs(start + 1_300L) > 0L);

        gate.onTickPose(start + 2_400L, true, 0.0, 64.0, 0.0, 9.0f, 0.0f);
        assertEquals(DataValidityGate.State.VALID, gate.state());
    }
}
