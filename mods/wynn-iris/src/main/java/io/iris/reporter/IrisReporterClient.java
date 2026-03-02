package io.iris.reporter;

import com.mojang.brigadier.arguments.BoolArgumentType;
import com.mojang.brigadier.arguments.StringArgumentType;
import com.mojang.brigadier.builder.LiteralArgumentBuilder;
import net.fabricmc.api.ClientModInitializer;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandManager;
import net.fabricmc.fabric.api.client.command.v2.ClientCommandRegistrationCallback;
import net.fabricmc.fabric.api.client.command.v2.FabricClientCommandSource;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.message.v1.ClientReceiveMessageEvents;
import net.minecraft.client.MinecraftClient;
import net.minecraft.text.MutableText;
import net.minecraft.text.Text;
import net.minecraft.util.Formatting;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.Locale;

public final class IrisReporterClient implements ClientModInitializer {
    public static final String MOD_VERSION = "0.1.0";
    public static final Logger LOGGER = LoggerFactory.getLogger("wynn-iris");
    private static final String ROOT_COMMAND = "iris";

    private static ReporterRuntime runtime;

    @Override
    public void onInitializeClient() {
        ReporterConfig config = ConfigStore.load();
        runtime = new ReporterRuntime(config);

        ClientTickEvents.END_CLIENT_TICK.register(client -> runtime.tick());
        ClientReceiveMessageEvents.GAME.register((message, overlay) ->
            runtime.onServerGameMessageSignal(message == null ? null : message.getString(), overlay)
        );

        registerCommands();

        LOGGER.info("Wynn Iris initialized");
    }

    private static void registerCommands() {
        ClientCommandRegistrationCallback.EVENT.register((dispatcher, registryAccess) -> {
            dispatcher.register(buildCommandTree(ROOT_COMMAND));
            dispatcher.register(buildCommandTree("irisreporter"));
            dispatcher.register(buildCommandTree("ir"));
        });
    }

    private static LiteralArgumentBuilder<FabricClientCommandSource> buildCommandTree(String root) {
        return ClientCommandManager.literal(root)
            .executes(context -> showHelp())
            .then(ClientCommandManager.literal("status").executes(context -> showStatus()))
            .then(ClientCommandManager.literal("scalar").executes(context -> showScalar()))
            .then(ClientCommandManager.literal("debug")
                .then(ClientCommandManager.literal("advancement")
                    .then(ClientCommandManager.argument("query", StringArgumentType.greedyString())
                        .executes(context -> {
                            String query = StringArgumentType.getString(context, "query");
                            return showAdvancementDebug(query);
                        }))))
            .then(ClientCommandManager.literal("toggles").executes(context -> showToggles()))
            .then(ClientCommandManager.literal("privacy").executes(context -> showPrivacy()))
            .then(ClientCommandManager.literal("help").executes(context -> showHelp()))
            .then(ClientCommandManager.literal("set-base-url")
                .then(ClientCommandManager.argument("url", StringArgumentType.greedyString())
                    .executes(context -> {
                        String url = StringArgumentType.getString(context, "url");
                        return setBaseUrl(url);
                    })))
            .then(ClientCommandManager.literal("toggle")
                .then(ClientCommandManager.argument("field", StringArgumentType.word())
                    .then(ClientCommandManager.argument("enabled", BoolArgumentType.bool())
                        .executes(context -> {
                            String field = StringArgumentType.getString(context, "field");
                            boolean enabled = BoolArgumentType.getBool(context, "enabled");
                            return toggleField(field, enabled);
                        }))));
    }

    private static int showHelp() {
        sendSection("Commands");
        sendKeyValue("status", commandText("/" + ROOT_COMMAND + " status"));
        sendKeyValue("scalar", commandText("/" + ROOT_COMMAND + " scalar"));
        sendKeyValue("debug", commandText("/" + ROOT_COMMAND + " debug advancement <territory>"));
        sendKeyValue("toggles", commandText("/" + ROOT_COMMAND + " toggles"));
        sendKeyValue("toggle", commandText("/" + ROOT_COMMAND + " toggle <field> <true|false>"));
        sendKeyValue("set_base_url", commandText("/" + ROOT_COMMAND + " set-base-url <url>"));
        sendKeyValue("privacy", commandText("/" + ROOT_COMMAND + " privacy"));
        sendKeyValue("alias", commandText("/ir <subcommand>"));
        sendKeyValue("compat", commandText("/irisreporter <subcommand>"));
        sendClientMessage(Text.literal("toggle fields: ").formatted(Formatting.GRAY)
            .append(Text.literal(
                "owner headquarters held_resources production_rates storage_capacity defense_tier "
                    + "trading_routes legacy_capture_signals legacy_war_signals"
            )
                .formatted(Formatting.AQUA)));
        return 1;
    }

    private static int showStatus() {
        sendSection("Status");
        sendKeyValue("enrolled", yesNoText(runtime.enrolled()));
        sendKeyValue("queue", queueText(runtime.queueSize()));
        sendKeyValue("state", stateText(runtime.runtimeState()));
        sendKeyValue("state_reason", Text.literal(runtime.runtimeStatusReason()).formatted(Formatting.GRAY));
        sendKeyValue("data_validity", validityText(runtime.dataValidityState()));
        sendKeyValue("pause_reason", Text.literal(runtime.dataValidityReason()).formatted(Formatting.GRAY));
        sendKeyValue("paused_for", Text.literal(runtime.dataValidityAge()).formatted(Formatting.GRAY));
        sendKeyValue("resume_progress", Text.literal(runtime.dataValidityResumeProgress()).formatted(Formatting.GRAY));
        sendKeyValue("ingest_base_url", Text.literal(runtime.ingestBaseUrl()).formatted(Formatting.GRAY));
        sendKeyValue("scalar_hint", Text.literal(runtime.scalarHintStatus()).formatted(Formatting.GRAY));
        sendKeyValue("last_upload", uploadStatusText(runtime.lastUploadStatus()));
        sendKeyValue("last_upload_at", Text.literal(runtime.lastUploadAt()).formatted(Formatting.GRAY));
        sendClientMessage(Text.literal("Use ").formatted(Formatting.GRAY)
            .append(commandText("/" + ROOT_COMMAND + " toggles"))
            .append(Text.literal(" for per-field sharing controls.").formatted(Formatting.GRAY)));
        return 1;
    }

    private static int showScalar() {
        sendSection("Scalar");
        ReporterRuntime.ScalarDebugSnapshot snapshot = runtime.scalarDebugSnapshot();
        if (!snapshot.available()) {
            sendKeyValue("state", Text.literal("unavailable").formatted(Formatting.RED));
            sendKeyValue("hint", Text.literal(snapshot.message()).formatted(Formatting.GRAY));
            return 0;
        }

        sendKeyValue("state", Text.literal("ready").formatted(Formatting.GREEN));
        sendKeyValue("season", Integer.toString(snapshot.seasonId()));
        sendKeyValue("territories", Integer.toString(snapshot.territories()));
        sendKeyValue("sr_per_hour", Integer.toString(snapshot.srPerHour()));
        sendKeyValue("weighted_units", decimalText(snapshot.weightedUnits()));
        sendKeyValue("scalar_weighted", decimalText(snapshot.weightedScalar()));
        sendKeyValue("scalar_raw", decimalText(snapshot.rawScalar()));
        sendKeyValue("age", Text.literal(snapshot.age()).formatted(Formatting.GRAY));
        sendClientMessage(Text.literal("formula ").formatted(Formatting.DARK_GRAY)
            .append(Text.literal("weighted=sr_per_hour/(120*weighted_units)").formatted(Formatting.GRAY)));
        sendClientMessage(Text.literal("formula ").formatted(Formatting.DARK_GRAY)
            .append(Text.literal("raw=sr_per_hour/(120*territories)").formatted(Formatting.GRAY)));
        return 1;
    }

    private static int showAdvancementDebug(String query) {
        String trimmed = query == null ? "" : query.trim();
        if (trimmed.isEmpty()) {
            sendClientMessage(Text.literal("usage: /" + ROOT_COMMAND + " debug advancement <territory>")
                .formatted(Formatting.YELLOW));
            return 0;
        }

        AdvancementTerritoryCollector.DebugLookupResult lookup = runtime.debugAdvancementTerritory(trimmed);
        if (lookup.kind() == AdvancementTerritoryCollector.DebugLookupKind.NO_MATCH) {
            sendClientMessage(Text.literal("no advancement territory matched '").formatted(Formatting.RED)
                .append(Text.literal(trimmed).formatted(Formatting.GOLD))
                .append(Text.literal("'").formatted(Formatting.RED)));
            return 0;
        }
        if (lookup.kind() == AdvancementTerritoryCollector.DebugLookupKind.AMBIGUOUS_MATCH) {
            sendClientMessage(Text.literal("query matched multiple territories. use an exact territory name:")
                .formatted(Formatting.YELLOW));
            for (String candidate : lookup.candidates()) {
                sendClientMessage(Text.literal(" - ").formatted(Formatting.DARK_GRAY)
                    .append(Text.literal(candidate).formatted(Formatting.AQUA)));
            }
            return 0;
        }

        AdvancementTerritoryCollector.DebugTerritoryData debug = lookup.data();
        if (debug == null) {
            sendClientMessage(Text.literal("no advancement territory matched '").formatted(Formatting.RED)
                .append(Text.literal(trimmed).formatted(Formatting.GOLD))
                .append(Text.literal("'").formatted(Formatting.RED)));
            return 0;
        }

        sendSection("Advancement Debug");
        sendKeyValue("query", Text.literal(trimmed).formatted(Formatting.GRAY));
        sendKeyValue("match", Text.literal(lookup.kind() == AdvancementTerritoryCollector.DebugLookupKind.EXACT_MATCH
            ? "exact"
            : "partial").formatted(Formatting.GRAY));
        sendKeyValue("territory", Text.literal(debug.territory()).formatted(Formatting.AQUA));
        String guild = debug.guildName().isEmpty()
            ? "n/a"
            : debug.guildName() + (debug.guildPrefix().isEmpty() ? "" : " [" + debug.guildPrefix() + "]");
        sendKeyValue("guild", Text.literal(guild).formatted(Formatting.GRAY));
        sendKeyValue("headquarters", yesNoText(debug.headquarters()));
        sendKeyValue("defense_tier", Text.literal(debug.defenseTier().isEmpty() ? "n/a" : debug.defenseTier()).formatted(Formatting.GRAY));
        sendKeyValue("held", Text.literal(formatResources(debug.heldResources())).formatted(Formatting.GRAY));
        sendKeyValue("storage", Text.literal(formatResources(debug.storageCapacity())).formatted(Formatting.GRAY));
        sendKeyValue("production", Text.literal(formatResources(debug.productionRates())).formatted(Formatting.GRAY));
        sendKeyValue("trading_routes", Text.literal(debug.tradingRoutes().isEmpty()
            ? "n/a"
            : String.join(", ", debug.tradingRoutes())).formatted(Formatting.GRAY));

        sendClientMessage(Text.literal("normalized lines:").formatted(Formatting.DARK_GRAY));
        int maxLines = Math.min(12, debug.normalizedLines().size());
        for (int idx = 0; idx < maxLines; idx++) {
            String line = debug.normalizedLines().get(idx);
            sendClientMessage(Text.literal("L" + (idx + 1) + ": ").formatted(Formatting.DARK_GRAY)
                .append(Text.literal(line).formatted(Formatting.GRAY)));
            sendClientMessage(Text.literal("  cp: ").formatted(Formatting.DARK_GRAY)
                .append(Text.literal(codepoints(line)).formatted(Formatting.DARK_GRAY)));
        }
        if (debug.normalizedLines().size() > maxLines) {
            sendClientMessage(Text.literal("...+" + (debug.normalizedLines().size() - maxLines) + " more lines").formatted(Formatting.DARK_GRAY));
        }

        return 1;
    }

    private static int showToggles() {
        sendSection("Sharing Toggles");
        sendKeyValue("owner", onOffText(runtime.shareOwner()));
        sendKeyValue("headquarters", onOffText(runtime.shareHeadquarters()));
        sendKeyValue("held_resources", onOffText(runtime.shareHeldResources()));
        sendKeyValue("production_rates", onOffText(runtime.shareProductionRates()));
        sendKeyValue("storage_capacity", onOffText(runtime.shareStorageCapacity()));
        sendKeyValue("defense_tier", onOffText(runtime.shareDefenseTier()));
        sendKeyValue("trading_routes", onOffText(runtime.shareTradingRoutes()));
        sendKeyValue("legacy_capture_signals", onOffText(runtime.shareLegacyCaptureSignals()));
        sendKeyValue("legacy_war_signals", onOffText(runtime.shareLegacyWarSignals()));
        return 1;
    }

    private static int showPrivacy() {
        sendSection("Privacy");
        sendKeyValue("source", Text.literal("advancement/map text only (public territory data)").formatted(Formatting.GREEN));
        sendKeyValue("not_shared_default", Text.literal("legacy chat-derived signals and route metadata remain off unless enabled").formatted(Formatting.YELLOW));
        sendKeyValue("extras", Text.literal("optional legacy scrapes are sent as runtime metadata only and are ignored by map logic")
            .formatted(Formatting.YELLOW));
        return 1;
    }

    private static int setBaseUrl(String url) {
        String previous = runtime.ingestBaseUrl();
        String error = runtime.setIngestBaseUrl(url);
        if (error != null) {
            sendClientMessage(Text.literal("invalid ingest base URL: ").formatted(Formatting.RED)
                .append(Text.literal(error).formatted(Formatting.GOLD)));
            sendClientMessage(Text.literal("example: https://seqwawa.com or https://seqwawa.com/*").formatted(Formatting.GRAY));
            return 0;
        }

        String current = runtime.ingestBaseUrl();
        if (previous != null && previous.equals(current)) {
            sendClientMessage(Text.literal("ingest base URL unchanged: ").formatted(Formatting.GRAY)
                .append(Text.literal(current).formatted(Formatting.AQUA)));
            return 1;
        }

        sendClientMessage(Text.literal("updated ingest base URL: ").formatted(Formatting.GRAY)
            .append(Text.literal(previous == null || previous.isBlank() ? "(unset)" : previous).formatted(Formatting.DARK_GRAY))
            .append(Text.literal(" -> ").formatted(Formatting.DARK_GRAY))
            .append(Text.literal(current).formatted(Formatting.AQUA)));
        sendClientMessage(Text.literal("token reset; reporter will re-enroll automatically.").formatted(Formatting.YELLOW));
        return 1;
    }

    private static int toggleField(String field, boolean enabled) {
        ReporterRuntime.ToggleResult result = runtime.setToggle(field, enabled);
        return switch (result.kind()) {
            case APPLIED -> {
                sendClientMessage(Text.literal("updated ").formatted(Formatting.GRAY)
                    .append(Text.literal(result.field()).formatted(Formatting.AQUA))
                    .append(Text.literal("=").formatted(Formatting.DARK_GRAY))
                    .append(onOffText(result.enabled())));
                showToggles();
                yield 1;
            }
            case UNKNOWN_FIELD -> {
                sendClientMessage(Text.literal("unknown toggle '").formatted(Formatting.RED)
                    .append(Text.literal(field).formatted(Formatting.GOLD))
                    .append(Text.literal("'").formatted(Formatting.RED)));
                sendClientMessage(Text.literal("valid: ").formatted(Formatting.GRAY)
                    .append(Text.literal(
                        "owner headquarters held_resources production_rates storage_capacity defense_tier "
                            + "trading_routes legacy_capture_signals legacy_war_signals"
                    )
                        .formatted(Formatting.AQUA)));
                yield 0;
            }
        };
    }

    private static String onOff(boolean enabled) {
        return enabled ? "on" : "off";
    }

    private static String yesNo(boolean value) {
        return value ? "yes" : "no";
    }

    private static void sendSection(String title) {
        sendClientMessage(Text.literal("=== ").formatted(Formatting.DARK_GRAY)
            .append(Text.literal(title).formatted(Formatting.LIGHT_PURPLE, Formatting.BOLD))
            .append(Text.literal(" ===").formatted(Formatting.DARK_GRAY)));
    }

    private static void sendKeyValue(String key, String value) {
        sendKeyValue(key, Text.literal(value).formatted(Formatting.GRAY));
    }

    private static void sendKeyValue(String key, Text value) {
        sendClientMessage(Text.literal("• ").formatted(Formatting.DARK_GRAY)
            .append(Text.literal(key).formatted(Formatting.AQUA))
            .append(Text.literal(": ").formatted(Formatting.DARK_GRAY))
            .append(value));
    }

    private static Text commandText(String command) {
        return Text.literal(command).formatted(Formatting.YELLOW);
    }

    private static Text onOffText(boolean enabled) {
        return Text.literal(onOff(enabled)).formatted(enabled ? Formatting.GREEN : Formatting.RED);
    }

    private static Text yesNoText(boolean value) {
        return Text.literal(yesNo(value)).formatted(value ? Formatting.GREEN : Formatting.RED);
    }

    private static Text queueText(int queueSize) {
        if (queueSize == 0) {
            return Text.literal("0").formatted(Formatting.GREEN);
        }
        if (queueSize < 4) {
            return Text.literal(Integer.toString(queueSize)).formatted(Formatting.YELLOW);
        }
        return Text.literal(Integer.toString(queueSize)).formatted(Formatting.RED);
    }

    private static Text stateText(String state) {
        Formatting color = switch (state) {
            case "upload_ok", "enrolled", "idle" -> Formatting.GREEN;
            case "upload_retry", "heartbeat_retry", "retrying", "queue_compacted", "enrolling", "recovering", "resyncing" -> Formatting.YELLOW;
            case "paused_afk", "paused_invalid_world" -> Formatting.RED;
            case "upload_reauth", "heartbeat_reauth", "enroll_failed" -> Formatting.RED;
            default -> Formatting.GRAY;
        };
        return Text.literal(state).formatted(color);
    }

    private static Text validityText(String validityState) {
        Formatting color = switch (validityState) {
            case "valid" -> Formatting.GREEN;
            case "recovering" -> Formatting.YELLOW;
            case "paused_afk", "paused_invalid_world" -> Formatting.RED;
            default -> Formatting.GRAY;
        };
        return Text.literal(validityState).formatted(color);
    }

    private static Text uploadStatusText(String status) {
        Formatting color = switch (status) {
            case "ok" -> Formatting.GREEN;
            case "retrying" -> Formatting.YELLOW;
            case "never" -> Formatting.GRAY;
            default -> Formatting.RED;
        };
        return Text.literal(status).formatted(color);
    }

    private static Text decimalText(double value) {
        if (!Double.isFinite(value)) {
            return Text.literal("n/a").formatted(Formatting.RED);
        }
        return Text.literal(String.format(Locale.ROOT, "%.4f", value)).formatted(Formatting.AQUA);
    }

    private static String formatResources(GatewayModels.Resources resources) {
        if (resources == null) {
            return "n/a";
        }
        return "em=" + resources.emeralds
            + " ore=" + resources.ore
            + " crops=" + resources.crops
            + " fish=" + resources.fish
            + " wood=" + resources.wood;
    }

    private static String codepoints(String value) {
        if (value == null || value.isEmpty()) {
            return "n/a";
        }
        StringBuilder out = new StringBuilder();
        value.codePoints().forEach(cp -> {
            if (out.length() > 0) {
                out.append(' ');
            }
            out.append(String.format(Locale.ROOT, "U+%04X", cp));
        });
        return out.toString();
    }

    private static void sendClientMessage(String text) {
        sendClientMessage(Text.literal(text).formatted(Formatting.GRAY));
    }

    private static void sendClientMessage(Text text) {
        MinecraftClient client = MinecraftClient.getInstance();
        if (client.player == null) {
            LOGGER.info(text.getString());
            return;
        }
        MutableText prefixed = Text.literal("[Iris] ").formatted(Formatting.AQUA, Formatting.BOLD)
            .append(text);
        client.player.sendMessage(prefixed, false);
    }

    static void onTitleSignal(String text) {
        if (runtime != null) {
            runtime.onTitleSignal(text);
        }
    }

    static void onSubtitleSignal(String text) {
        if (runtime != null) {
            runtime.onSubtitleSignal(text);
        }
    }

    static void onTitleClearSignal() {
        if (runtime != null) {
            runtime.onTitleClearSignal();
        }
    }

    static void onWorldSignal(String packetType, String details) {
        if (runtime != null) {
            runtime.onWorldSignal(packetType, details);
        }
    }
}
