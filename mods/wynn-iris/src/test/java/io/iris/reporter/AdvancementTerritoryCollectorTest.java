package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class AdvancementTerritoryCollectorTest {
    @Test
    void ownerParsingRequiresExplicitOwnerLabel() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Nivla Woods",
            "Not owner text [FAKE]\\nTerritory Defense: Strong",
            false
        );

        assertTrue(debug.guildName().isEmpty());
        assertTrue(debug.guildPrefix().isEmpty());
    }

    @Test
    void parsesTradingRoutesFromLegacyBulletLines() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Nivla Woods",
            "Owner: Sequoia [SEQ]\n"
                + "- Ragni Plains\n"
                + "• Katoa Ranch\n"
                + "route: Ternaves",
            false
        );

        assertEquals(3, debug.tradingRoutes().size());
        assertEquals("Ragni Plains", debug.tradingRoutes().get(0));
        assertEquals("Katoa Ranch", debug.tradingRoutes().get(1));
        assertEquals("Ternaves", debug.tradingRoutes().get(2));
    }

    @Test
    void parsesDecimalAndSuffixProductionValues() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Nivla Woods",
            "Owner: Sequoia [SEQ]\\n"
                + "+1.2k Emeralds per Hour\\n"
                + "+2.5k Ⓒ per hour\\n"
                + "+5.6k ✦ per hour\\n"
                + "+4.5k Ⓙ per hour\\n"
                + "+3.4k Ⓚ per hour",
            false
        );

        assertEquals(1200, debug.productionRates().emeralds);
        assertEquals(2500, debug.productionRates().ore);
        assertEquals(5600, debug.productionRates().crops);
        assertEquals(4500, debug.productionRates().fish);
        assertEquals(3400, debug.productionRates().wood);
    }

    @Test
    void storageFallbackDoesNotOverrideTrustedMarkerParsedValues() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Nivla Woods",
            "Owner: Sequoia [SEQ]\\n"
                + "Ⓑ 10/100 stored\\n"
                + "Ⓒ 20/200 stored\\n"
                + "✦ 30/300 stored\\n"
                + "Ⓙ 40/400 stored\\n"
                + "Ⓚ 50/500 stored\\n"
                + "999/9999 stored\\n"
                + "888/8888 stored\\n"
                + "777/7777 stored\\n"
                + "666/6666 stored\\n"
                + "555/5555 stored",
            false
        );

        assertEquals(10, debug.heldResources().emeralds);
        assertEquals(20, debug.heldResources().ore);
        assertEquals(30, debug.heldResources().crops);
        assertEquals(40, debug.heldResources().fish);
        assertEquals(50, debug.heldResources().wood);

        assertEquals(100, debug.storageCapacity().emeralds);
        assertEquals(200, debug.storageCapacity().ore);
        assertEquals(300, debug.storageCapacity().crops);
        assertEquals(400, debug.storageCapacity().fish);
        assertEquals(500, debug.storageCapacity().wood);
    }

    @Test
    void partialNamedStorageLinesDoNotShiftResources() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Sequoia",
            "Owner: Sequoia [SEQ]\n"
                + "+9900 Emeralds per Hour\n"
                + "326537/5000 Emeralds stored\n"
                + "⛏ +3960 Ore per Hour\n"
                + "⛏ 76128/120000 Ore stored\n"
                + "🌾 9525/120000 Crops stored\n"
                + "Territory Defences: Very High",
            true
        );

        assertEquals(326537, debug.heldResources().emeralds);
        assertEquals(76128, debug.heldResources().ore);
        assertEquals(9525, debug.heldResources().crops);
        assertEquals(0, debug.heldResources().fish);
        assertEquals(0, debug.heldResources().wood);

        assertEquals(5000, debug.storageCapacity().emeralds);
        assertEquals(120000, debug.storageCapacity().ore);
        assertEquals(120000, debug.storageCapacity().crops);
        assertEquals(0, debug.storageCapacity().fish);
        assertEquals(0, debug.storageCapacity().wood);
    }

    @Test
    void iconOnlyStorageLinesInferResourceFromIcon() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Sequoia",
            "⛏ 76128/120000 stored\n"
                + "🌾 9525/120000 stored\n"
                + "🐟 400/1200 stored\n"
                + "🪵 700/1800 stored\n"
                + "Ⓑ 10000/50000 stored",
            false
        );

        assertEquals(10000, debug.heldResources().emeralds);
        assertEquals(76128, debug.heldResources().ore);
        assertEquals(9525, debug.heldResources().crops);
        assertEquals(400, debug.heldResources().fish);
        assertEquals(700, debug.heldResources().wood);
    }

    @Test
    void unlabeledStorageRatiosUsePositionalFallbackOrder() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Sequoia",
            "76128/120000 stored\n"
                + "9525/120000 stored\n"
                + "400/1200 stored\n"
                + "700/1800 stored\n"
                + "10000/50000 stored",
            false
        );

        assertEquals(76128, debug.heldResources().emeralds);
        assertEquals(9525, debug.heldResources().ore);
        assertEquals(10000, debug.heldResources().crops);
        assertEquals(700, debug.heldResources().fish);
        assertEquals(400, debug.heldResources().wood);
    }

    @Test
    void ambiguousMarkerStorageLinesAreAssignedWithoutFishCropShift() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Corkus City Crossroads",
            "+9900 Emeralds per Hour\n"
                + "132/3000 stored\n"
                + "Ⓒ +3960 Fish per Hour\n"
                + "Ⓑ 9/300 stored\n"
                + "Ⓒ 7/300 stored\n"
                + "Ⓚ 59/300 stored\n"
                + "Ⓙ 5/300 stored",
            false
        );

        assertEquals(132, debug.heldResources().emeralds);
        assertEquals(0, debug.heldResources().ore);
        assertEquals(0, debug.heldResources().crops);
        assertEquals(0, debug.heldResources().fish);
        assertEquals(0, debug.heldResources().wood);

        assertEquals(3000, debug.storageCapacity().emeralds);
        assertEquals(0, debug.storageCapacity().ore);
        assertEquals(0, debug.storageCapacity().crops);
        assertEquals(0, debug.storageCapacity().fish);
        assertEquals(0, debug.storageCapacity().wood);

        assertEquals(3960, debug.productionRates().fish);
    }

    @Test
    void alternateProfileInfersOreWoodFishCropsFromSingleFishMarkerHint() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Corkus City",
            "+9900 Emeralds per Hour\n"
                + "132/3000 stored\n"
                + "Ⓚ +3960 Fish per Hour\n"
                + "Ⓑ 100/300 stored\n"
                + "Ⓒ 200/300 stored\n"
                + "Ⓚ 300/300 stored\n"
                + "Ⓙ 5/300 stored",
            false
        );

        assertEquals(132, debug.heldResources().emeralds);
        assertEquals(100, debug.heldResources().ore);
        assertEquals(200, debug.heldResources().wood);
        assertEquals(300, debug.heldResources().fish);
        assertEquals(5, debug.heldResources().crops);

        assertEquals(3000, debug.storageCapacity().emeralds);
        assertEquals(300, debug.storageCapacity().ore);
        assertEquals(300, debug.storageCapacity().wood);
        assertEquals(300, debug.storageCapacity().fish);
        assertEquals(300, debug.storageCapacity().crops);
    }

    @Test
    void debugLookupPrefersExactMatchOverPartialMatch() {
        AdvancementTerritoryCollector.DebugQuerySelection selection =
            AdvancementTerritoryCollector.selectDebugTerritoryQuery(
                "Corkus City",
                List.of("Corkus City Crossroads", "Corkus City")
            );

        assertEquals(AdvancementTerritoryCollector.DebugLookupKind.EXACT_MATCH, selection.kind());
        assertEquals("Corkus City", selection.selectedTerritory());
        assertTrue(selection.candidates().isEmpty());
    }

    @Test
    void debugLookupReturnsAmbiguousMatchesForPartialQuery() {
        AdvancementTerritoryCollector.DebugQuerySelection selection =
            AdvancementTerritoryCollector.selectDebugTerritoryQuery(
                "Corkus",
                List.of("Upper Corkus City", "Corkus City Crossroads", "Corkus City")
            );

        assertEquals(AdvancementTerritoryCollector.DebugLookupKind.AMBIGUOUS_MATCH, selection.kind());
        assertTrue(selection.selectedTerritory() == null);
        assertEquals(
            List.of("Corkus City", "Corkus City Crossroads", "Upper Corkus City"),
            selection.candidates()
        );
    }

    @Test
    void debugLookupReturnsNoMatchWhenNothingMatches() {
        AdvancementTerritoryCollector.DebugQuerySelection selection =
            AdvancementTerritoryCollector.selectDebugTerritoryQuery(
                "Detlas",
                List.of("Corkus City", "Corkus City Crossroads")
            );

        assertEquals(AdvancementTerritoryCollector.DebugLookupKind.NO_MATCH, selection.kind());
        assertTrue(selection.selectedTerritory() == null);
        assertTrue(selection.candidates().isEmpty());
    }

    @Test
    void namedStorageLinesTakePriorityOverUnknownMarkers() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Cinfras Outskirts",
            "⚑ 11044/5000 Emeralds stored\n"
                + "⚑ 104775/1500 Ore stored\n"
                + "⚑ 100882/1500 Wood stored\n"
                + "⚑ 114210/1500 Fish stored\n"
                + "⚑ 3413/1500 Crops stored",
            false
        );

        assertEquals(11044, debug.heldResources().emeralds);
        assertEquals(104775, debug.heldResources().ore);
        assertEquals(100882, debug.heldResources().wood);
        assertEquals(114210, debug.heldResources().fish);
        assertEquals(3413, debug.heldResources().crops);

        assertEquals(5000, debug.storageCapacity().emeralds);
        assertEquals(1500, debug.storageCapacity().ore);
        assertEquals(1500, debug.storageCapacity().wood);
        assertEquals(1500, debug.storageCapacity().fish);
        assertEquals(1500, debug.storageCapacity().crops);
    }

    @Test
    void storageMarkersUseAlternateGlyphSetWhenHintedByProductionLine() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Bloody Trail",
            "Ⓑ +3960 Ore per Hour\n"
                + "Ⓑ 2/1500 stored\n"
                + "Ⓚ 2/1500 stored",
            false
        );

        assertEquals(3960, debug.productionRates().ore);
        assertEquals(2, debug.heldResources().ore);
        assertEquals(2, debug.heldResources().fish);
        assertEquals(0, debug.heldResources().wood);

        assertEquals(1500, debug.storageCapacity().ore);
        assertEquals(1500, debug.storageCapacity().fish);
        assertEquals(0, debug.storageCapacity().wood);
    }

    @Test
    void mixedGlyphStorageOrderKeepsFishAndCropsSeparate() {
        AdvancementTerritoryCollector.DebugTerritoryData debug = AdvancementTerritoryCollector.parseDebugFromDescription(
            "Cinfras Outskirts",
            "+9900 Emeralds per Hour\n"
                + "399491/400000 stored\n"
                + "Ⓑ 119781/120000 stored\n"
                + "Ⓒ +3960 Wood per Hour\n"
                + "Ⓒ 119357/120000 stored\n"
                + "Ⓚ 119728/120000 stored\n"
                + "Ⓙ 66255/120000 stored",
            false
        );

        assertEquals(399491, debug.heldResources().emeralds);
        assertEquals(119781, debug.heldResources().ore);
        assertEquals(119357, debug.heldResources().wood);
        assertEquals(119728, debug.heldResources().fish);
        assertEquals(66255, debug.heldResources().crops);
    }
}
