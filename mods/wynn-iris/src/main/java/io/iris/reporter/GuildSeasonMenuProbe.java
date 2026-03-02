package io.iris.reporter;

import net.minecraft.client.MinecraftClient;
import net.minecraft.client.gui.screen.Screen;
import net.minecraft.client.gui.screen.ingame.HandledScreen;
import net.minecraft.item.Item;
import net.minecraft.item.ItemStack;
import net.minecraft.item.tooltip.TooltipType;
import net.minecraft.registry.Registries;
import net.minecraft.screen.ScreenHandler;
import net.minecraft.screen.slot.Slot;
import net.minecraft.text.Text;

import java.time.Instant;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Captures season SR metadata from Wynncraft guild-menu item tooltips.
 */
public final class GuildSeasonMenuProbe {
    private static final long SCAN_INTERVAL_MS = 1_000L;
    private static final long OBSERVATION_TTL_MS = 120_000L;
    private static final int SCALAR_SLOT_ID = 11;
    private static final String SCALAR_ITEM_ID = "MINECRAFT:BLAZE_POWDER";
    private static final Pattern GUILD_MANAGE_MENU_PATTERN = Pattern.compile(
        "^[^:]{2,64}\\s*:\\s*MANAGE$",
        Pattern.CASE_INSENSITIVE
    );

    private static final Pattern SEASON_PATTERN = Pattern.compile(
        "SEASON\\s+(\\d+)\\s+STATUS",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern CAPTURED_TERRITORIES_PATTERN = Pattern.compile(
        "CAPTURED\\s+TERRITORIES\\s*:\\s*([0-9][0-9,]*)",
        Pattern.CASE_INSENSITIVE
    );
    private static final Pattern SR_PER_HOUR_PATTERN = Pattern.compile(
        "\\+?([0-9][0-9,]*)\\s+SR\\s+PER\\s+HOUR",
        Pattern.CASE_INSENSITIVE
    );

    private long lastScanMs;
    private Observation latestObservation;

    public void tick(long nowMs) {
        if (nowMs - lastScanMs < SCAN_INTERVAL_MS) {
            return;
        }
        lastScanMs = nowMs;

        Observation observed = scanCurrentScreen(nowMs);
        if (observed != null) {
            latestObservation = observed;
        }
    }

    public Observation latestFreshObservation(long nowMs) {
        if (latestObservation == null) {
            return null;
        }
        if (nowMs - latestObservation.observedAtMs > OBSERVATION_TTL_MS) {
            return null;
        }
        return latestObservation;
    }

    static Observation parseObservationFromLines(List<String> lines, long observedAtMs) {
        if (lines == null || lines.isEmpty()) {
            return null;
        }

        Integer seasonId = null;
        Integer capturedTerritories = null;
        Integer srPerHour = null;

        for (String raw : lines) {
            String normalized = normalize(raw);
            if (normalized.isEmpty()) {
                continue;
            }

            if (seasonId == null) {
                Matcher matcher = SEASON_PATTERN.matcher(normalized);
                if (matcher.find()) {
                    seasonId = parseNumber(matcher.group(1));
                }
            }
            if (capturedTerritories == null) {
                Matcher matcher = CAPTURED_TERRITORIES_PATTERN.matcher(normalized);
                if (matcher.find()) {
                    capturedTerritories = parseNumber(matcher.group(1));
                }
            }
            if (srPerHour == null) {
                Matcher matcher = SR_PER_HOUR_PATTERN.matcher(normalized);
                if (matcher.find()) {
                    srPerHour = parseNumber(matcher.group(1));
                }
            }
        }

        if (seasonId == null || capturedTerritories == null || srPerHour == null) {
            return null;
        }
        if (seasonId <= 0 || capturedTerritories < 0 || srPerHour < 0) {
            return null;
        }

        return new Observation(seasonId, capturedTerritories, srPerHour, observedAtMs);
    }

    private static Observation scanCurrentScreen(long nowMs) {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null || client.player == null) {
            return null;
        }

        Screen screen = client.currentScreen;
        if (!(screen instanceof HandledScreen<?> handledScreen)) {
            return null;
        }

        String menuTitle = normalize(handledScreen.getTitle().getString());
        if (!isAllowedMenuTitle(menuTitle)) {
            return null;
        }

        ScreenHandler handler = handledScreen.getScreenHandler();
        if (handler == null || handler.slots == null || handler.slots.isEmpty()) {
            return null;
        }

        for (Slot slot : handler.slots) {
            if (slot == null) {
                continue;
            }
            ItemStack stack = slot.getStack();
            if (stack == null || stack.isEmpty()) {
                continue;
            }
            String itemId = normalizeItemId(Registries.ITEM.getId(stack.getItem()).toString());
            if (!isAllowedScalarItem(itemId, slot.id)) {
                continue;
            }

            List<Text> tooltip = stack.getTooltip(Item.TooltipContext.DEFAULT, client.player, TooltipType.BASIC);
            if (tooltip == null || tooltip.isEmpty()) {
                continue;
            }

            List<String> lines = tooltip.stream()
                .map(Text::getString)
                .toList();
            Observation parsed = parseObservationFromLines(lines, nowMs);
            if (parsed != null) {
                return parsed;
            }
        }

        return null;
    }

    static boolean isAllowedMenuTitle(String normalizedMenuTitle) {
        if (normalizedMenuTitle == null || normalizedMenuTitle.isBlank()) {
            return false;
        }
        return GUILD_MANAGE_MENU_PATTERN.matcher(normalizedMenuTitle).find();
    }

    static boolean isAllowedScalarItem(String normalizedItemId, int slotId) {
        return slotId == SCALAR_SLOT_ID && SCALAR_ITEM_ID.equals(normalizedItemId);
    }

    private static int parseNumber(String value) {
        if (value == null || value.isBlank()) {
            return 0;
        }
        try {
            return Integer.parseInt(value.replace(",", ""));
        } catch (NumberFormatException ignored) {
            return 0;
        }
    }

    private static String normalize(String value) {
        if (value == null || value.isBlank()) {
            return "";
        }
        return value
            .toUpperCase(Locale.ROOT)
            .replaceAll("§[0-9A-FK-ORX]", " ")
            .replaceAll("\\s+", " ")
            .trim();
    }

    private static String normalizeItemId(String value) {
        if (value == null || value.isBlank()) {
            return "";
        }
        return value.trim().toUpperCase(Locale.ROOT);
    }

    public static final class Observation {
        private final int seasonId;
        private final int capturedTerritories;
        private final int srPerHour;
        private final long observedAtMs;

        private Observation(int seasonId, int capturedTerritories, int srPerHour, long observedAtMs) {
            this.seasonId = seasonId;
            this.capturedTerritories = capturedTerritories;
            this.srPerHour = srPerHour;
            this.observedAtMs = observedAtMs;
        }

        public int seasonId() {
            return seasonId;
        }

        public int capturedTerritories() {
            return capturedTerritories;
        }

        public int srPerHour() {
            return srPerHour;
        }

        public long observedAtMs() {
            return observedAtMs;
        }

        public void attachToProvenance(Map<String, Object> provenance) {
            if (provenance == null) {
                return;
            }
            provenance.put("menu_season_id", seasonId);
            provenance.put("menu_captured_territories", capturedTerritories);
            provenance.put("menu_sr_per_hour", srPerHour);
            provenance.put("menu_observed_at", Instant.ofEpochMilli(observedAtMs).toString());
        }
    }
}
