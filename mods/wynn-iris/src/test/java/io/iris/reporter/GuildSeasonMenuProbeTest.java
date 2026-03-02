package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class GuildSeasonMenuProbeTest {
    @Test
    void parsesSeasonTooltipSnapshotFromGuildMenuText() {
        long observedAt = 1_700_000_000_000L;
        List<String> lines = List.of(
            "Season 29 Status",
            "Season will end in 9 hours",
            "Captured Territories: 64",
            "+30061 SR per Hour"
        );

        GuildSeasonMenuProbe.Observation observation =
            GuildSeasonMenuProbe.parseObservationFromLines(lines, observedAt);

        assertNotNull(observation);
        assertEquals(29, observation.seasonId());
        assertEquals(64, observation.capturedTerritories());
        assertEquals(30061, observation.srPerHour());
        assertEquals(observedAt, observation.observedAtMs());
    }

    @Test
    void returnsNullWithoutAllRequiredSignals() {
        GuildSeasonMenuProbe.Observation observation =
            GuildSeasonMenuProbe.parseObservationFromLines(
                List.of("Captured Territories: 12", "Nothing else useful"),
                42L
            );
        assertNull(observation);
    }

    @Test
    void acceptsLegitimateZeroValueSeasonState() {
        GuildSeasonMenuProbe.Observation observation =
            GuildSeasonMenuProbe.parseObservationFromLines(
                List.of(
                    "Season 30 Status",
                    "Captured Territories: 0",
                    "+0 SR per Hour"
                ),
                7L
            );
        assertNotNull(observation);
        assertEquals(0, observation.capturedTerritories());
        assertEquals(0, observation.srPerHour());
    }

    @Test
    void menuWhitelistIsGuildAgnostic() {
        assertTrue(GuildSeasonMenuProbe.isAllowedMenuTitle("SEQUOIA: MANAGE"));
        assertTrue(GuildSeasonMenuProbe.isAllowedMenuTitle("ANOTHER GUILD: MANAGE"));
        assertFalse(GuildSeasonMenuProbe.isAllowedMenuTitle("SEQUOIA: SETTINGS"));
        assertFalse(GuildSeasonMenuProbe.isAllowedMenuTitle("REWARD TYPES"));
    }

    @Test
    void scalarItemWhitelistRequiresExpectedItemAndSlot() {
        assertTrue(GuildSeasonMenuProbe.isAllowedScalarItem("MINECRAFT:BLAZE_POWDER", 11));
        assertFalse(GuildSeasonMenuProbe.isAllowedScalarItem("MINECRAFT:BLAZE_POWDER", 10));
        assertFalse(GuildSeasonMenuProbe.isAllowedScalarItem("MINECRAFT:SNOW", 11));
    }
}
