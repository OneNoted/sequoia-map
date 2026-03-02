package io.iris.reporter;

import net.minecraft.advancement.Advancement;
import net.minecraft.advancement.AdvancementDisplay;
import net.minecraft.advancement.AdvancementFrame;
import net.minecraft.advancement.AdvancementManager;
import net.minecraft.advancement.PlacedAdvancement;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.network.ClientAdvancementManager;
import net.minecraft.client.network.ClientPlayNetworkHandler;
import net.minecraft.text.Text;

import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public final class AdvancementTerritoryCollector {
    public record DebugTerritoryData(
        String territory,
        String guildName,
        String guildPrefix,
        boolean headquarters,
        String defenseTier,
        GatewayModels.Resources heldResources,
        GatewayModels.Resources storageCapacity,
        GatewayModels.Resources productionRates,
        List<String> tradingRoutes,
        List<String> rawLines,
        List<String> normalizedLines
    ) {
    }

    public enum DebugLookupKind {
        EXACT_MATCH,
        PARTIAL_MATCH,
        AMBIGUOUS_MATCH,
        NO_MATCH
    }

    public record DebugLookupResult(
        DebugLookupKind kind,
        DebugTerritoryData data,
        List<String> candidates
    ) {
        static DebugLookupResult exact(DebugTerritoryData data) {
            return new DebugLookupResult(DebugLookupKind.EXACT_MATCH, data, List.of());
        }

        static DebugLookupResult partial(DebugTerritoryData data) {
            return new DebugLookupResult(DebugLookupKind.PARTIAL_MATCH, data, List.of());
        }

        static DebugLookupResult ambiguous(List<String> candidates) {
            return new DebugLookupResult(DebugLookupKind.AMBIGUOUS_MATCH, null, List.copyOf(candidates));
        }

        static DebugLookupResult noMatch() {
            return new DebugLookupResult(DebugLookupKind.NO_MATCH, null, List.of());
        }
    }

    record DebugQuerySelection(
        DebugLookupKind kind,
        String selectedTerritory,
        List<String> candidates
    ) {
        static DebugQuerySelection exact(String territory) {
            return new DebugQuerySelection(DebugLookupKind.EXACT_MATCH, territory, List.of());
        }

        static DebugQuerySelection partial(String territory) {
            return new DebugQuerySelection(DebugLookupKind.PARTIAL_MATCH, territory, List.of());
        }

        static DebugQuerySelection ambiguous(List<String> candidates) {
            return new DebugQuerySelection(DebugLookupKind.AMBIGUOUS_MATCH, null, List.copyOf(candidates));
        }

        static DebugQuerySelection noMatch() {
            return new DebugQuerySelection(DebugLookupKind.NO_MATCH, null, List.of());
        }
    }

    private static final String RESOURCE_MARKERS = "ⒷⒸⓀⒿ✦";
    private static final int HINT_WEIGHT_PRODUCTION = 1;
    private static final int HINT_WEIGHT_STORAGE_NAMED = 4;
    private static final String NUMERIC_CAPTURE = "([0-9][0-9,._\\s]*(?:\\.[0-9]+)?[kKmMbB]?)";
    private static final Pattern OWNER_PATTERN = Pattern.compile(
        "^(?:[-*•]\\s*)?(?:territory\\s+)?owner\\s*:?\\s*(?<name>[^\\[]+?)\\s*\\[(?<tag>[A-Za-z0-9]{1,6})]\\s*$",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern PRODUCTION_PATTERN = Pattern.compile(
        "\\+" + NUMERIC_CAPTURE + "\\s*(Emeralds|Ore|Wood|Fish|Crops)\\s+per\\s+Hour",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern PRODUCTION_LEADING_MARKER_PATTERN = Pattern.compile(
        "^\\s*([" + RESOURCE_MARKERS + "])\\s*\\+" + NUMERIC_CAPTURE
            + "\\s*(Emeralds|Ore|Wood|Fish|Crops)\\s+per\\s+Hour",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern PRODUCTION_MARKER_PATTERN = Pattern.compile(
        "\\+" + NUMERIC_CAPTURE + "\\s*([" + RESOURCE_MARKERS + "])(?:\\s+per\\s+hour)?",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern PRODUCTION_AMOUNT_PATTERN = Pattern.compile(
        "\\+" + NUMERIC_CAPTURE,
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern PER_HOUR_HINT_PATTERN = Pattern.compile(
        "\\bPER\\s+HOUR\\b",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern STORAGE_PATTERN = Pattern.compile(
        "(?:([" + RESOURCE_MARKERS + "])\\s*)?" + NUMERIC_CAPTURE + "\\s*/\\s*" + NUMERIC_CAPTURE
            + "\\s+stored(?:\\s*([" + RESOURCE_MARKERS + "]))?",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern STORAGE_NAMED_PATTERN = Pattern.compile(
        "(?:([" + RESOURCE_MARKERS + "])\\s*)?" + NUMERIC_CAPTURE + "\\s*/\\s*" + NUMERIC_CAPTURE
            + "\\s*(Emeralds|Ore|Wood|Fish|Crops)\\s+stored(?:\\s*([" + RESOURCE_MARKERS + "]))?",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern STORAGE_MARKER_RATIO_PATTERN = Pattern.compile(
        "(?:([" + RESOURCE_MARKERS + "])\\s*)?" + NUMERIC_CAPTURE + "\\s*/\\s*" + NUMERIC_CAPTURE
            + "(?:\\s*([" + RESOURCE_MARKERS + "]))?",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern STORAGE_FALLBACK_RATIO_PATTERN = Pattern.compile(
        NUMERIC_CAPTURE + "\\s*/\\s*" + NUMERIC_CAPTURE,
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern DEFENSE_PATTERN = Pattern.compile(
        "(?:Territory\\s+)?Defen(?:ce|se)(?:\\s+Tier)?s?\\s*:\\s*(.+)",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern MINECRAFT_FORMAT_CODE_PATTERN = Pattern.compile(
        "§[0-9A-FK-ORX]",
        Pattern.CASE_INSENSITIVE
    );
    // Storage lines are rendered in this canonical order when labels are omitted.
    private static final String[] STORAGE_POSITIONAL_ORDER = { "emeralds", "ore", "wood", "fish", "crops" };

    public List<GatewayModels.TerritoryUpdate> collect(ReporterConfig config) {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null) {
            return List.of();
        }

        ClientPlayNetworkHandler networkHandler = client.getNetworkHandler();
        if (networkHandler == null) {
            return List.of();
        }

        ClientAdvancementManager advancementHandler = networkHandler.getAdvancementHandler();
        if (advancementHandler == null) {
            return List.of();
        }

        AdvancementManager manager = advancementHandler.getManager();
        if (manager == null) {
            return List.of();
        }

        Collection<PlacedAdvancement> advancements = manager.getAdvancements();
        if (advancements == null || advancements.isEmpty()) {
            return List.of();
        }

        List<GatewayModels.TerritoryUpdate> updates = new ArrayList<>();
        Set<String> dedupe = new HashSet<>();
        for (PlacedAdvancement placedAdvancement : advancements) {
            Advancement advancement = placedAdvancement.getAdvancement();
            if (advancement == null) {
                continue;
            }

            AdvancementDisplay display = advancement.display().orElse(null);
            if (display == null) {
                continue;
            }

            String territory = normalize(textToString(display.getTitle()));
            if (territory.isEmpty() || !dedupe.add(territory)) {
                continue;
            }

            String description = textToString(display.getDescription());
            ParsedTerritory parsed = parseTerritory(territory, description, isHeadquarters(display));

            GatewayModels.TerritoryUpdate update = buildUpdate(parsed, config);
            if (update != null) {
                updates.add(update);
            }
        }

        return updates;
    }

    public DebugLookupResult debugAdvancementTerritory(String query) {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null) {
            return DebugLookupResult.noMatch();
        }

        ClientPlayNetworkHandler networkHandler = client.getNetworkHandler();
        if (networkHandler == null) {
            return DebugLookupResult.noMatch();
        }

        ClientAdvancementManager advancementHandler = networkHandler.getAdvancementHandler();
        if (advancementHandler == null) {
            return DebugLookupResult.noMatch();
        }

        AdvancementManager manager = advancementHandler.getManager();
        if (manager == null) {
            return DebugLookupResult.noMatch();
        }

        Collection<PlacedAdvancement> advancements = manager.getAdvancements();
        if (advancements == null || advancements.isEmpty()) {
            return DebugLookupResult.noMatch();
        }

        Map<String, DebugCandidate> candidateByTerritory = new LinkedHashMap<>();
        for (PlacedAdvancement placedAdvancement : advancements) {
            Advancement advancement = placedAdvancement.getAdvancement();
            if (advancement == null) {
                continue;
            }

            AdvancementDisplay display = advancement.display().orElse(null);
            if (display == null) {
                continue;
            }

            String territory = normalize(textToString(display.getTitle()));
            if (territory.isEmpty()) {
                continue;
            }

            String description = textToString(display.getDescription());
            candidateByTerritory.putIfAbsent(
                territory,
                new DebugCandidate(description, isHeadquarters(display))
            );
        }

        DebugQuerySelection selection = selectDebugTerritoryQuery(query, candidateByTerritory.keySet());
        return switch (selection.kind()) {
            case EXACT_MATCH -> {
                String territory = selection.selectedTerritory();
                DebugCandidate candidate = candidateByTerritory.get(territory);
                if (territory == null || candidate == null) {
                    yield DebugLookupResult.noMatch();
                }
                yield DebugLookupResult.exact(
                    parseDebugFromDescription(territory, candidate.description(), candidate.headquarters())
                );
            }
            case PARTIAL_MATCH -> {
                String territory = selection.selectedTerritory();
                DebugCandidate candidate = candidateByTerritory.get(territory);
                if (territory == null || candidate == null) {
                    yield DebugLookupResult.noMatch();
                }
                yield DebugLookupResult.partial(
                    parseDebugFromDescription(territory, candidate.description(), candidate.headquarters())
                );
            }
            case AMBIGUOUS_MATCH -> DebugLookupResult.ambiguous(selection.candidates());
            case NO_MATCH -> DebugLookupResult.noMatch();
        };
    }

    static DebugQuerySelection selectDebugTerritoryQuery(String query, Collection<String> territoryNames) {
        if (territoryNames == null || territoryNames.isEmpty()) {
            return DebugQuerySelection.noMatch();
        }
        String needleNormalized = normalize(query);
        if (needleNormalized.isEmpty()) {
            return DebugQuerySelection.noMatch();
        }
        String needleLower = needleNormalized.toLowerCase(Locale.ROOT);

        List<String> exactMatches = new ArrayList<>();
        List<String> partialMatches = new ArrayList<>();
        for (String territory : territoryNames) {
            if (territory == null || territory.isBlank()) {
                continue;
            }
            String normalizedTerritory = normalize(territory);
            if (normalizedTerritory.equalsIgnoreCase(needleNormalized)) {
                exactMatches.add(normalizedTerritory);
                continue;
            }
            if (normalizedTerritory.toLowerCase(Locale.ROOT).contains(needleLower)) {
                partialMatches.add(normalizedTerritory);
            }
        }

        if (exactMatches.size() == 1) {
            return DebugQuerySelection.exact(exactMatches.get(0));
        }
        if (exactMatches.size() > 1) {
            return DebugQuerySelection.ambiguous(sortCaseInsensitive(exactMatches));
        }
        if (partialMatches.size() == 1) {
            return DebugQuerySelection.partial(partialMatches.get(0));
        }
        if (partialMatches.size() > 1) {
            return DebugQuerySelection.ambiguous(sortCaseInsensitive(partialMatches));
        }
        return DebugQuerySelection.noMatch();
    }

    private static List<String> sortCaseInsensitive(List<String> values) {
        List<String> sorted = new ArrayList<>(values);
        sorted.sort(String.CASE_INSENSITIVE_ORDER);
        return sorted;
    }

    static DebugTerritoryData parseDebugFromDescription(String territory, String description, boolean headquarters) {
        String normalizedTerritory = normalize(territory);
        ParsedTerritory parsed = parseTerritory(normalizedTerritory, description, headquarters);
        List<String> rawLines = (description == null || description.isBlank())
            ? List.of()
            : List.of(description.split("\\R"));
        List<String> normalizedLines = rawLines.stream()
            .map(AdvancementTerritoryCollector::normalize)
            .filter(line -> !line.isEmpty())
            .toList();

        return new DebugTerritoryData(
            normalizedTerritory,
            parsed.guildName,
            parsed.guildPrefix,
            parsed.headquarters,
            parsed.defenseTier,
            parsed.storedResources.copy(),
            parsed.storageCapacity.copy(),
            parsed.productionRates.copy(),
            List.copyOf(parsed.tradingRoutes),
            rawLines,
            normalizedLines
        );
    }

    private static GatewayModels.TerritoryUpdate buildUpdate(ParsedTerritory parsed, ReporterConfig config) {
        GatewayModels.TerritoryUpdate update = new GatewayModels.TerritoryUpdate();
        update.territory = parsed.name;

        if (config.shareOwner && !parsed.guildName.isEmpty() && !parsed.guildPrefix.isEmpty()) {
            Map<String, Object> guild = new HashMap<>();
            guild.put("uuid", "");
            guild.put("name", parsed.guildName);
            guild.put("prefix", parsed.guildPrefix);
            update.guild = guild;
        }

        GatewayModels.RuntimeData runtime = new GatewayModels.RuntimeData();
        if (config.shareHeadquarters && parsed.headquarters) {
            runtime.headquarters = true;
        }
        if (config.shareHeldResources && !parsed.storedResources.isEmpty()) {
            runtime.held_resources = parsed.storedResources.copy();
        }
        if (config.shareProductionRates && !parsed.productionRates.isEmpty()) {
            runtime.production_rates = parsed.productionRates.copy();
        }
        if (config.shareStorageCapacity && !parsed.storageCapacity.isEmpty()) {
            runtime.storage_capacity = parsed.storageCapacity.copy();
        }
        if (config.shareDefenseTier && !parsed.defenseTier.isEmpty()) {
            runtime.defense_tier = parsed.defenseTier;
        }
        if (config.shareTradingRoutes && !parsed.tradingRoutes.isEmpty()) {
            runtime.extra_scrapes = new HashMap<>();
            runtime.extra_scrapes.put("trading_routes", List.copyOf(parsed.tradingRoutes));
        }

        if (hasRuntimeData(runtime)) {
            update.runtime = runtime;
        }

        if (update.guild == null && update.runtime == null) {
            return null;
        }

        return update;
    }

    private static boolean hasRuntimeData(GatewayModels.RuntimeData runtime) {
        return Boolean.TRUE.equals(runtime.headquarters)
            || runtime.held_resources != null
            || runtime.production_rates != null
            || runtime.storage_capacity != null
            || runtime.defense_tier != null
            || (runtime.extra_scrapes != null && !runtime.extra_scrapes.isEmpty());
    }

    private static ParsedTerritory parseTerritory(String territory, String description, boolean headquarters) {
        ParsedTerritory parsed = new ParsedTerritory();
        parsed.name = territory;
        parsed.headquarters = headquarters;
        parsed.productionRates = GatewayModels.Resources.of(0, 0, 0, 0, 0);
        parsed.storedResources = GatewayModels.Resources.of(0, 0, 0, 0, 0);
        parsed.storageCapacity = GatewayModels.Resources.of(0, 0, 0, 0, 0);
        List<Integer> fallbackProductionAmounts = new ArrayList<>(5);
        List<int[]> unresolvedUnmarkedStorageRatios = new ArrayList<>(5);
        List<PendingMarkerAmount> pendingMarkerProductionAmounts = new ArrayList<>(5);
        List<PendingMarkerRatio> pendingMarkerStorageRatios = new ArrayList<>(5);
        List<MarkerHint> markerProfileHints = new ArrayList<>(8);
        Map<Character, String> productionMarkerHints = new HashMap<>(5);
        Map<Character, String> namedStorageMarkerHints = new HashMap<>(5);

        if (description == null || description.isBlank()) {
            return parsed;
        }

        String[] lines = description.split("\\R");
        for (String rawLine : lines) {
            String line = normalize(rawLine);
            if (line.isEmpty()) {
                continue;
            }

            if (parsed.guildName.isEmpty()) {
                Matcher owner = OWNER_PATTERN.matcher(line);
                if (owner.matches()) {
                    parsed.guildName = normalize(owner.group("name"));
                    parsed.guildPrefix = normalize(owner.group("tag"));
                    continue;
                }
            }

            Matcher defense = DEFENSE_PATTERN.matcher(line);
            if (defense.find()) {
                parsed.defenseTier = normalize(defense.group(1));
            }

            boolean productionMatched = false;
            Matcher production = PRODUCTION_PATTERN.matcher(line);
            while (production.find()) {
                int amount = parseNumber(production.group(1));
                String resource = production.group(2).toLowerCase(Locale.ROOT);
                setByName(parsed.productionRates, resource, amount);
                productionMatched = true;
            }

            Matcher productionLeadingMarker = PRODUCTION_LEADING_MARKER_PATTERN.matcher(line);
            while (productionLeadingMarker.find()) {
                String marker = productionLeadingMarker.group(1);
                String resource = productionLeadingMarker.group(3).toLowerCase(Locale.ROOT);
                if (marker != null && !marker.isBlank()) {
                    char markerChar = marker.charAt(0);
                    markerProfileHints.add(new MarkerHint(markerChar, resource, HINT_WEIGHT_PRODUCTION));
                    productionMarkerHints.put(markerChar, resource);
                }
            }

            Matcher productionMarker = PRODUCTION_MARKER_PATTERN.matcher(line);
            while (productionMarker.find()) {
                int amount = parseNumber(productionMarker.group(1));
                String marker = productionMarker.group(2);
                if (marker != null && !marker.isBlank()) {
                    pendingMarkerProductionAmounts.add(new PendingMarkerAmount(marker.charAt(0), amount));
                    productionMatched = true;
                }
            }

            if (!productionMatched && PER_HOUR_HINT_PATTERN.matcher(line).find()) {
                Matcher productionAmount = PRODUCTION_AMOUNT_PATTERN.matcher(line);
                if (productionAmount.find()) {
                    fallbackProductionAmounts.add(parseNumber(productionAmount.group(1)));
                }
            }

            String route = extractTradingRoute(line);
            if (route != null) {
                parsed.tradingRoutes.add(route);
                continue;
            }

            boolean maybeStorageLine =
                line.contains("/") && !PER_HOUR_HINT_PATTERN.matcher(line).find();
            if (!maybeStorageLine) {
                continue;
            }
            String inferredStorageResource = inferResourceFromLine(line);

            boolean storageMatched = false;
            Matcher namedStorage = STORAGE_NAMED_PATTERN.matcher(line);
            while (namedStorage.find()) {
                int current = parseNumber(namedStorage.group(2));
                int max = parseNumber(namedStorage.group(3));
                String resource = namedStorage.group(4).toLowerCase(Locale.ROOT);
                setByNameMax(parsed.storedResources, resource, current);
                setByNameMax(parsed.storageCapacity, resource, max);
                String marker = firstNonEmpty(namedStorage.group(1), namedStorage.group(5));
                if (marker != null && !marker.isBlank()) {
                    char markerChar = marker.charAt(0);
                    markerProfileHints.add(new MarkerHint(markerChar, resource, HINT_WEIGHT_STORAGE_NAMED));
                    namedStorageMarkerHints.put(markerChar, resource);
                }
                storageMatched = true;
            }
            if (storageMatched) {
                // Resource names are authoritative for this line; do not run marker/positional
                // fallback paths that can reassign values when marker glyphs are ambiguous.
                continue;
            }

            Matcher storage = STORAGE_PATTERN.matcher(line);
            while (storage.find()) {
                String marker = firstNonEmpty(storage.group(1), storage.group(4));
                int current = parseNumber(storage.group(2));
                int max = parseNumber(storage.group(3));
                if (marker == null || marker.isBlank()) {
                    if (inferredStorageResource != null) {
                        setByNameMax(parsed.storedResources, inferredStorageResource, current);
                        setByNameMax(parsed.storageCapacity, inferredStorageResource, max);
                    } else {
                        unresolvedUnmarkedStorageRatios.add(new int[] { current, max });
                    }
                } else {
                    pendingMarkerStorageRatios.add(new PendingMarkerRatio(marker.charAt(0), current, max));
                }
                storageMatched = true;
            }
            if (!storageMatched) {
                Matcher storageMarkerRatio = STORAGE_MARKER_RATIO_PATTERN.matcher(line);
                while (storageMarkerRatio.find()) {
                    String marker = firstNonEmpty(storageMarkerRatio.group(1), storageMarkerRatio.group(4));
                    int current = parseNumber(storageMarkerRatio.group(2));
                    int max = parseNumber(storageMarkerRatio.group(3));
                    if (marker == null || marker.isBlank()) {
                        if (inferredStorageResource != null) {
                            setByNameMax(parsed.storedResources, inferredStorageResource, current);
                            setByNameMax(parsed.storageCapacity, inferredStorageResource, max);
                        } else {
                            unresolvedUnmarkedStorageRatios.add(new int[] { current, max });
                        }
                    } else {
                        pendingMarkerStorageRatios.add(new PendingMarkerRatio(marker.charAt(0), current, max));
                    }
                    storageMatched = true;
                }
            }

            // Resource-specific scrape path for icon-based or mixed-format lines.
            // This avoids positional shifting when only a subset of storage lines is present.
            if (!storageMatched) {
                Matcher ratio = STORAGE_FALLBACK_RATIO_PATTERN.matcher(line);
                if (ratio.find()) {
                    int current = parseNumber(ratio.group(1));
                    int max = parseNumber(ratio.group(2));
                    if (inferredStorageResource != null) {
                        setByNameMax(parsed.storedResources, inferredStorageResource, current);
                        setByNameMax(parsed.storageCapacity, inferredStorageResource, max);
                    } else {
                        unresolvedUnmarkedStorageRatios.add(new int[] { current, max });
                    }
                    storageMatched = true;
                }
            }
        }

        MarkerProfileSelection profileSelection = selectMarkerProfile(markerProfileHints);
        applyPendingProductionMarkers(
            parsed.productionRates,
            pendingMarkerProductionAmounts,
            productionMarkerHints,
            profileSelection
        );
        applyPendingStorageMarkers(
            parsed,
            pendingMarkerStorageRatios,
            namedStorageMarkerHints,
            profileSelection
        );

        if (parsed.productionRates.isEmpty() && fallbackProductionAmounts.size() == 5) {
            parsed.productionRates.emeralds = fallbackProductionAmounts.get(0);
            parsed.productionRates.ore = fallbackProductionAmounts.get(1);
            parsed.productionRates.crops = fallbackProductionAmounts.get(2);
            parsed.productionRates.fish = fallbackProductionAmounts.get(3);
            parsed.productionRates.wood = fallbackProductionAmounts.get(4);
        }
        applyPositionalStorageFallback(parsed, unresolvedUnmarkedStorageRatios);

        return parsed;
    }

    private static int parseNumber(String value) {
        if (value == null || value.isBlank()) {
            return 0;
        }
        String normalized = value
            .replace('\u00A0', ' ')
            .replace('\u202F', ' ')
            .replace('_', ' ')
            .trim()
            .replace(",", "")
            .replaceAll("\\s+", "");
        if (normalized.isEmpty()) {
            return 0;
        }

        double multiplier = 1.0;
        char suffix = Character.toLowerCase(normalized.charAt(normalized.length() - 1));
        if (suffix == 'k' || suffix == 'm' || suffix == 'b') {
            normalized = normalized.substring(0, normalized.length() - 1);
            multiplier = switch (suffix) {
                case 'k' -> 1_000.0;
                case 'm' -> 1_000_000.0;
                case 'b' -> 1_000_000_000.0;
                default -> 1.0;
            };
        }

        if (normalized.isEmpty()) {
            return 0;
        }

        try {
            double parsed = Double.parseDouble(normalized) * multiplier;
            if (parsed >= Integer.MAX_VALUE) {
                return Integer.MAX_VALUE;
            }
            if (parsed <= Integer.MIN_VALUE) {
                return Integer.MIN_VALUE;
            }
            return (int) Math.round(parsed);
        } catch (NumberFormatException ignored) {
            return 0;
        }
    }

    private static String firstNonEmpty(String a, String b) {
        if (a != null && !a.isBlank()) {
            return a;
        }
        if (b != null && !b.isBlank()) {
            return b;
        }
        return null;
    }

    private static void setByName(GatewayModels.Resources resources, String resource, int value) {
        switch (resource) {
            case "emeralds" -> resources.emeralds = value;
            case "ore" -> resources.ore = value;
            case "crops" -> resources.crops = value;
            case "fish" -> resources.fish = value;
            case "wood" -> resources.wood = value;
            default -> {
            }
        }
    }

    private static void setByNameMax(GatewayModels.Resources resources, String resource, int value) {
        switch (resource) {
            case "emeralds" -> resources.emeralds = Math.max(resources.emeralds, value);
            case "ore" -> resources.ore = Math.max(resources.ore, value);
            case "crops" -> resources.crops = Math.max(resources.crops, value);
            case "fish" -> resources.fish = Math.max(resources.fish, value);
            case "wood" -> resources.wood = Math.max(resources.wood, value);
            default -> {
            }
        }
    }

    private static void applyPendingProductionMarkers(
        GatewayModels.Resources productionRates,
        List<PendingMarkerAmount> pendingMarkerProductionAmounts,
        Map<Character, String> productionMarkerHints,
        MarkerProfileSelection profileSelection
    ) {
        if (pendingMarkerProductionAmounts.isEmpty()) {
            return;
        }
        for (PendingMarkerAmount pending : pendingMarkerProductionAmounts) {
            String resource = resolveProductionMarkerResource(pending.marker(), productionMarkerHints, profileSelection);
            if (resource != null) {
                setByName(productionRates, resource, pending.amount());
            }
        }
    }

    private static void applyPendingStorageMarkers(
        ParsedTerritory parsed,
        List<PendingMarkerRatio> pendingMarkerStorageRatios,
        Map<Character, String> namedStorageMarkerHints,
        MarkerProfileSelection profileSelection
    ) {
        if (pendingMarkerStorageRatios.isEmpty()) {
            return;
        }
        for (PendingMarkerRatio pending : pendingMarkerStorageRatios) {
            String resource = resolveStorageMarkerResource(
                pending.marker(),
                namedStorageMarkerHints,
                profileSelection
            );
            if (resource != null) {
                setByNameMax(parsed.storedResources, resource, pending.current());
                setByNameMax(parsed.storageCapacity, resource, pending.max());
            }
        }
    }

    private static String resolveProductionMarkerResource(
        char marker,
        Map<Character, String> productionMarkerHints,
        MarkerProfileSelection profileSelection
    ) {
        String hinted = productionMarkerHints.get(marker);
        if (hinted != null && !hinted.isBlank()) {
            return hinted;
        }
        if (!profileSelection.confident()) {
            return null;
        }
        return profileSelection.profile().resourceFor(marker);
    }

    private static String resolveStorageMarkerResource(
        char marker,
        Map<Character, String> namedStorageMarkerHints,
        MarkerProfileSelection profileSelection
    ) {
        String hinted = namedStorageMarkerHints.get(marker);
        if (hinted != null && !hinted.isBlank()) {
            return hinted;
        }
        if (!profileSelection.confident()) {
            return null;
        }
        return profileSelection.profile().resourceFor(marker);
    }

    private static MarkerProfileSelection selectMarkerProfile(List<MarkerHint> markerHints) {
        if (markerHints.isEmpty()) {
            return new MarkerProfileSelection(MarkerProfile.DEFAULT, true, 0, 0);
        }

        List<MarkerProfileScore> scores = new ArrayList<>(MarkerProfile.values().length);
        for (MarkerProfile profile : MarkerProfile.values()) {
            scores.add(scoreMarkerProfile(profile, markerHints));
        }

        MarkerProfileScore bestZeroContradictions = null;
        for (MarkerProfileScore score : scores) {
            if (score.contradictions() != 0) {
                continue;
            }
            if (bestZeroContradictions == null || score.matches() > bestZeroContradictions.matches()) {
                bestZeroContradictions = score;
            }
        }
        if (bestZeroContradictions != null) {
            boolean confident = bestZeroContradictions.matches() > 0;
            return new MarkerProfileSelection(
                bestZeroContradictions.profile(),
                confident,
                bestZeroContradictions.matches(),
                bestZeroContradictions.contradictions()
            );
        }

        MarkerProfileScore bestScore = scores.get(0);
        for (int idx = 1; idx < scores.size(); idx++) {
            MarkerProfileScore current = scores.get(idx);
            if (current.contradictions() < bestScore.contradictions()) {
                bestScore = current;
                continue;
            }
            if (current.contradictions() == bestScore.contradictions()
                && current.matches() > bestScore.matches()) {
                bestScore = current;
            }
        }

        return new MarkerProfileSelection(
            bestScore.profile(),
            false,
            bestScore.matches(),
            bestScore.contradictions()
        );
    }

    private static MarkerProfileScore scoreMarkerProfile(MarkerProfile profile, List<MarkerHint> markerHints) {
        int matches = 0;
        int contradictions = 0;
        for (MarkerHint hint : markerHints) {
            String mapped = profile.resourceFor(hint.marker());
            if (mapped == null) {
                contradictions += hint.weight();
                continue;
            }
            if (mapped.equals(hint.resource())) {
                matches += hint.weight();
            } else {
                contradictions += hint.weight();
            }
        }
        return new MarkerProfileScore(profile, matches, contradictions);
    }

    private static void applyPositionalStorageFallback(ParsedTerritory parsed, List<int[]> unresolvedRatios) {
        if (unresolvedRatios.isEmpty()) {
            return;
        }
        for (int[] ratio : unresolvedRatios) {
            String target = nextUnsetStorageResource(parsed);
            if (target == null) {
                return;
            }
            setByNameMax(parsed.storedResources, target, ratio[0]);
            setByNameMax(parsed.storageCapacity, target, ratio[1]);
        }
    }

    private static String nextUnsetStorageResource(ParsedTerritory parsed) {
        for (String resource : STORAGE_POSITIONAL_ORDER) {
            if (!hasStorageValue(parsed.storedResources, parsed.storageCapacity, resource)) {
                return resource;
            }
        }
        return null;
    }

    private static boolean hasStorageValue(
        GatewayModels.Resources held,
        GatewayModels.Resources capacity,
        String resource
    ) {
        return switch (resource) {
            case "emeralds" -> held.emeralds > 0 || capacity.emeralds > 0;
            case "ore" -> held.ore > 0 || capacity.ore > 0;
            case "wood" -> held.wood > 0 || capacity.wood > 0;
            case "fish" -> held.fish > 0 || capacity.fish > 0;
            case "crops" -> held.crops > 0 || capacity.crops > 0;
            default -> true;
        };
    }

    private static String inferResourceFromLine(String line) {
        String lower = line.toLowerCase(Locale.ROOT);
        if (containsAsciiWord(lower, "emerald") || containsAsciiWord(lower, "emeralds")) {
            return "emeralds";
        }
        if (containsAsciiWord(lower, "ore")) {
            return "ore";
        }
        if (containsAsciiWord(lower, "crop") || containsAsciiWord(lower, "crops")) {
            return "crops";
        }
        if (containsAsciiWord(lower, "fish")) {
            return "fish";
        }
        if (containsAsciiWord(lower, "wood")) {
            return "wood";
        }

        // Alternate icon set observed in some tooltip variants.
        if (line.contains("⛏")) {
            return "ore";
        }
        if (line.contains("🌾")) {
            return "crops";
        }
        if (line.contains("🐟")) {
            return "fish";
        }
        if (line.contains("🪵")) {
            return "wood";
        }

        return null;
    }

    private static boolean containsAsciiWord(String text, String needle) {
        if (text == null || text.isEmpty() || needle == null || needle.isEmpty()) {
            return false;
        }
        String[] tokens = text.split("[^a-z]+");
        for (String token : tokens) {
            if (needle.equals(token)) {
                return true;
            }
        }
        return false;
    }

    private static String extractTradingRoute(String line) {
        if (line == null || line.isEmpty()) {
            return null;
        }

        if (line.startsWith("- ") || line.startsWith("• ") || line.startsWith("· ")) {
            String route = normalize(line.substring(2));
            return route.isEmpty() ? null : route;
        }

        String lower = line.toLowerCase(Locale.ROOT);
        if (lower.startsWith("route:")) {
            String route = normalize(line.substring(line.indexOf(':') + 1));
            return route.isEmpty() ? null : route;
        }
        if (lower.startsWith("route ")) {
            String route = normalize(line.substring("route".length()));
            return route.isEmpty() ? null : route;
        }

        return null;
    }

    private static boolean isHeadquarters(AdvancementDisplay display) {
        AdvancementFrame frame = display.getFrame();
        return frame != null && "CHALLENGE".equalsIgnoreCase(frame.name());
    }

    private static String textToString(Text maybeText) {
        if (maybeText == null) {
            return "";
        }
        return maybeText.getString();
    }

    private static String normalize(String value) {
        if (value == null) {
            return "";
        }
        String stripped = MINECRAFT_FORMAT_CODE_PATTERN.matcher(value).replaceAll("");
        return stripped.trim().replaceAll("\\s+", " ");
    }

    private record DebugCandidate(
        String description,
        boolean headquarters
    ) {
    }

    private record PendingMarkerAmount(
        char marker,
        int amount
    ) {
    }

    private record PendingMarkerRatio(
        char marker,
        int current,
        int max
    ) {
    }

    private record MarkerHint(
        char marker,
        String resource,
        int weight
    ) {
    }

    private record MarkerProfileScore(
        MarkerProfile profile,
        int matches,
        int contradictions
    ) {
    }

    private record MarkerProfileSelection(
        MarkerProfile profile,
        boolean confident,
        int matches,
        int contradictions
    ) {
    }

    private enum MarkerProfile {
        DEFAULT {
            @Override
            String resourceFor(char marker) {
                return switch (marker) {
                    case 'Ⓑ' -> "emeralds";
                    case 'Ⓒ' -> "ore";
                    case '✦' -> "crops";
                    case 'Ⓙ' -> "fish";
                    case 'Ⓚ' -> "wood";
                    default -> null;
                };
            }
        },
        ALTERNATE {
            @Override
            String resourceFor(char marker) {
                return switch (marker) {
                    case 'Ⓑ' -> "ore";
                    case 'Ⓒ' -> "wood";
                    case '✦' -> "crops";
                    case 'Ⓙ' -> "crops";
                    case 'Ⓚ' -> "fish";
                    default -> null;
                };
            }
        };

        abstract String resourceFor(char marker);
    }

    private static final class ParsedTerritory {
        private String name = "";
        private String guildName = "";
        private String guildPrefix = "";
        private boolean headquarters;
        private String defenseTier = "";
        private GatewayModels.Resources productionRates;
        private GatewayModels.Resources storedResources;
        private GatewayModels.Resources storageCapacity;
        private final List<String> tradingRoutes = new ArrayList<>();
    }
}
