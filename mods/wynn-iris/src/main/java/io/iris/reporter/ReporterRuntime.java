package io.iris.reporter;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import net.minecraft.client.MinecraftClient;
import net.minecraft.client.network.ClientPlayerEntity;

import java.time.Instant;
import java.time.ZoneId;
import java.time.format.DateTimeFormatter;
import java.util.ArrayDeque;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;

public final class ReporterRuntime {
    private static final Gson GSON = new GsonBuilder().create();
    private static final long ENROLL_RETRY_MS = 15_000;
    private static final long HEARTBEAT_MS = 60_000;
    private static final long ADVANCEMENT_SCAN_MS = 10_000;
    private static final long UPLOAD_DISPATCH_INTERVAL_MS = 200;
    private static final long CONFIG_SAVE_DEBOUNCE_MS = 3_000;
    private static final int QUEUE_COALESCE_THRESHOLD = 4;
    // Keep canonical runtime overrides fresh even when values don't change.
    private static final long PERIODIC_REFRESH_MS = 30_000;
    private static final long LEGACY_SIGNAL_TTL_MS = 300_000;
    private static final DateTimeFormatter STATUS_LOCAL_TIME_FORMATTER = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss");

    public enum ToggleResultKind {
        APPLIED,
        UNKNOWN_FIELD
    }

    public enum UpdateCheckStartResult {
        STARTED,
        ALREADY_RUNNING,
        INVALID_REPO
    }

    public enum UpdateApplyStartResult {
        STARTED,
        ALREADY_RUNNING,
        NO_PENDING_UPDATE,
        INVALID_ASSET_URL,
        INVALID_PENDING_HASH
    }

    public enum UpdateNotificationKind {
        UPDATE_AVAILABLE,
        UPDATE_UP_TO_DATE,
        UPDATE_NO_COMPATIBLE_RELEASE,
        UPDATE_CHECK_FAILED,
        UPDATE_APPLY_STAGED,
        UPDATE_APPLY_SUCCESS,
        UPDATE_APPLY_FAILED
    }

    public record UpdateNotification(
        UpdateNotificationKind kind,
        String currentVersion,
        String latestVersion,
        String releaseUrl,
        String reason
    ) {}

    public static final class ToggleResult {
        private final ToggleResultKind kind;
        private final String field;
        private final boolean enabled;
        private final String message;

        private ToggleResult(ToggleResultKind kind, String field, boolean enabled, String message) {
            this.kind = kind;
            this.field = field;
            this.enabled = enabled;
            this.message = message;
        }

        public static ToggleResult applied(String field, boolean enabled) {
            return new ToggleResult(ToggleResultKind.APPLIED, field, enabled, null);
        }

        public static ToggleResult unknownField(String field) {
            return new ToggleResult(ToggleResultKind.UNKNOWN_FIELD, field, false, null);
        }

        public ToggleResultKind kind() {
            return kind;
        }

        public String field() {
            return field;
        }

        public boolean enabled() {
            return enabled;
        }

        public String message() {
            return message;
        }
    }

    private final ReporterConfig config;
    private final GatewayClient gatewayClient;
    private final IrisAutoUpdater autoUpdater;
    private final AdvancementTerritoryCollector collector;
    private final GuildSeasonMenuProbe guildSeasonMenuProbe;
    private final DataValidityGate validityGate;
    private final ArrayDeque<PendingSubmission> queue = new ArrayDeque<>();
    private final ArrayDeque<UpdateNotification> updateNotifications = new ArrayDeque<>();
    private final Map<String, String> fingerprintByTerritory = new HashMap<>();
    private final Map<String, Long> lastSentAtByTerritory = new HashMap<>();
    private final Map<String, LegacyMessageScraper.CaptureSignal> legacyCaptureSignalsByTerritory = new HashMap<>();
    private final Map<String, LegacyMessageScraper.WarSignal> legacyWarSignalsByTerritory = new HashMap<>();

    private long lastEnrollAttemptMs;
    private long lastHeartbeatMs;
    private long lastAdvancementScanMs;
    private long lastUploadDispatchMs;
    private long nextConfigFlushMs;
    private String lastStatus = "idle";
    private String lastStatusReason = "n/a";
    private boolean configDirty;
    private CompletableFuture<GatewayClient.EnrollResult> enrollInFlight;
    private CompletableFuture<GatewayClient.HeartbeatResult> heartbeatInFlight;
    private CompletableFuture<GatewayClient.SubmitResult> uploadInFlight;
    private CompletableFuture<IrisAutoUpdater.CheckResult> updateCheckInFlight;
    private CompletableFuture<IrisAutoUpdater.ApplyResult> updateApplyInFlight;
    private UpdateCheckContext updateCheckContext;
    private PendingSubmission uploadHeadInFlight;
    private DataValidityGate.State lastValidityState;
    private boolean startupUpdateCheckTriggered;
    private String pendingUpdateReleaseUrl;
    private String updateApplyTargetVersion;

    public ReporterRuntime(ReporterConfig config) {
        this.config = config;
        this.gatewayClient = new GatewayClient();
        this.autoUpdater = new IrisAutoUpdater();
        this.collector = new AdvancementTerritoryCollector();
        this.guildSeasonMenuProbe = new GuildSeasonMenuProbe();
        this.validityGate = new DataValidityGate(config.resumeStabilizationMs, config.minMovementBlocks);
        this.lastValidityState = validityGate.state();
        this.config.autoUpdateRepo = IrisAutoUpdater.normalizeRepo(this.config.autoUpdateRepo);
        reconcilePendingUpdateJobOnStartup();
    }

    public String statusLine() {
        return "enrolled=" + (config.token != null && !config.token.isBlank())
            + " queue=" + queue.size()
            + " last_upload=" + config.lastUploadStatus
            + " validity=" + validityGate.stateId()
            + " last=" + lastStatus
            + " reason=" + lastStatusReason;
    }

    public String togglesLine() {
        return "share_owner=" + config.shareOwner
            + " share_headquarters=" + config.shareHeadquarters
            + " share_held_resources=" + config.shareHeldResources
            + " share_production_rates=" + config.shareProductionRates
            + " share_storage_capacity=" + config.shareStorageCapacity
            + " share_defense_tier=" + config.shareDefenseTier
            + " share_trading_routes=" + config.shareTradingRoutes
            + " share_legacy_capture_signals=" + config.shareLegacyCaptureSignals
            + " share_legacy_war_signals=" + config.shareLegacyWarSignals;
    }

    public String ingestBaseUrl() {
        return config.ingestBaseUrl;
    }

    public boolean autoUpdateEnabled() {
        return config.autoUpdateEnabled;
    }

    public boolean autoUpdateIncludePrerelease() {
        return config.autoUpdateIncludePrerelease;
    }

    public String autoUpdateRepo() {
        return IrisAutoUpdater.normalizeRepo(config.autoUpdateRepo);
    }

    public String autoUpdateLastResult() {
        if (config.autoUpdateLastResult == null || config.autoUpdateLastResult.isBlank()) {
            return "never";
        }
        return config.autoUpdateLastResult;
    }

    public String autoUpdateLastCheckAt() {
        if (config.autoUpdateLastCheckAt == null || config.autoUpdateLastCheckAt.isBlank()) {
            return "never";
        }
        try {
            Instant parsed = Instant.parse(config.autoUpdateLastCheckAt);
            String local = STATUS_LOCAL_TIME_FORMATTER.format(parsed.atZone(ZoneId.systemDefault()));
            long ageMs = Math.max(0L, System.currentTimeMillis() - parsed.toEpochMilli());
            return local + " (" + formatDuration(ageMs) + " ago)";
        } catch (Exception ignored) {
            return config.autoUpdateLastCheckAt;
        }
    }

    public String autoUpdatePendingVersion() {
        if (config.autoUpdatePendingVersion == null || config.autoUpdatePendingVersion.isBlank()) {
            return "none";
        }
        return config.autoUpdatePendingVersion;
    }

    public String autoUpdateApplyState() {
        if (config.autoUpdateApplyState == null || config.autoUpdateApplyState.isBlank()) {
            return "idle";
        }
        return config.autoUpdateApplyState;
    }

    public String autoUpdateLastApplyReason() {
        if (config.autoUpdateLastApplyReason == null || config.autoUpdateLastApplyReason.isBlank()) {
            return "never";
        }
        return config.autoUpdateLastApplyReason;
    }

    public String autoUpdateLastApplyAt() {
        if (config.autoUpdateLastApplyAt == null || config.autoUpdateLastApplyAt.isBlank()) {
            return "never";
        }
        try {
            Instant parsed = Instant.parse(config.autoUpdateLastApplyAt);
            String local = STATUS_LOCAL_TIME_FORMATTER.format(parsed.atZone(ZoneId.systemDefault()));
            long ageMs = Math.max(0L, System.currentTimeMillis() - parsed.toEpochMilli());
            return local + " (" + formatDuration(ageMs) + " ago)";
        } catch (Exception ignored) {
            return config.autoUpdateLastApplyAt;
        }
    }

    public String autoUpdatePendingReleaseUrl() {
        if (pendingUpdateReleaseUrl == null || pendingUpdateReleaseUrl.isBlank()) {
            return "n/a";
        }
        return pendingUpdateReleaseUrl;
    }

    public boolean updateCheckInProgress() {
        return updateCheckInFlight != null;
    }

    public boolean updateApplyInProgress() {
        return updateApplyInFlight != null;
    }

    public String runtimeModVersion() {
        return RuntimeVersionResolver.currentModVersion();
    }

    public String runtimeMinecraftVersion() {
        return RuntimeVersionResolver.currentMinecraftVersion();
    }

    public boolean setAutoUpdateEnabled(boolean enabled) {
        if (config.autoUpdateEnabled == enabled) {
            return false;
        }
        config.autoUpdateEnabled = enabled;
        markConfigDirty(System.currentTimeMillis());
        return true;
    }

    public String setIngestBaseUrl(String ingestBaseUrl) {
        String normalized = normalizeIngestBaseUrlInput(ingestBaseUrl);
        String validationError = GatewayClient.ingestUrlValidationError(normalized, config.allowInsecureIngestHttp);
        if (validationError != null) {
            return validationError;
        }
        if (Objects.equals(config.ingestBaseUrl, normalized)) {
            return null;
        }

        config.ingestBaseUrl = normalized;
        GatewayClient.invalidateToken(config);
        lastEnrollAttemptMs = 0L;
        setStatus("enrolling", "base_url_changed");
        return null;
    }

    static String normalizeIngestBaseUrlInput(String ingestBaseUrl) {
        if (ingestBaseUrl == null) {
            return "";
        }

        String normalized = ingestBaseUrl.trim();
        while (!normalized.isEmpty() && (normalized.endsWith("*") || normalized.endsWith("/"))) {
            normalized = normalized.substring(0, normalized.length() - 1);
        }
        return normalized;
    }

    public ToggleResult setToggle(String field, boolean enabled) {
        if (field == null || field.isBlank()) {
            return ToggleResult.unknownField("");
        }

        String normalized = field.trim().toLowerCase();
        switch (normalized) {
            case "owner", "share_owner" -> config.shareOwner = enabled;
            case "headquarters", "hq", "share_headquarters" -> config.shareHeadquarters = enabled;
            case "held_resources", "held", "share_held_resources" -> config.shareHeldResources = enabled;
            case "production_rates", "production", "share_production_rates" -> config.shareProductionRates = enabled;
            case "storage_capacity", "capacity", "share_storage_capacity" -> config.shareStorageCapacity = enabled;
            case "defense_tier", "defense", "share_defense_tier" -> config.shareDefenseTier = enabled;
            case "trading_routes", "routes", "connections", "share_trading_routes" -> config.shareTradingRoutes = enabled;
            case "legacy_capture_signals", "capture_signals", "share_legacy_capture_signals" ->
                config.shareLegacyCaptureSignals = enabled;
            case "legacy_war_signals", "war_signals", "share_legacy_war_signals" ->
                config.shareLegacyWarSignals = enabled;
            default -> {
                return ToggleResult.unknownField(normalized);
            }
        }

        ConfigStore.save(config);
        return ToggleResult.applied(normalized, enabled);
    }

    public boolean enrolled() {
        return config.token != null && !config.token.isBlank();
    }

    public int queueSize() {
        return queue.size();
    }

    public String lastUploadStatus() {
        return config.lastUploadStatus == null ? "never" : config.lastUploadStatus;
    }

    public String lastUploadAt() {
        if (config.lastUploadAt == null || config.lastUploadAt.isBlank()) {
            return "never";
        }
        try {
            Instant parsed = Instant.parse(config.lastUploadAt);
            String local = STATUS_LOCAL_TIME_FORMATTER.format(parsed.atZone(ZoneId.systemDefault()));
            long ageMs = Math.max(0L, System.currentTimeMillis() - parsed.toEpochMilli());
            return local + " (" + formatDuration(ageMs) + " ago)";
        } catch (Exception ignored) {
            return config.lastUploadAt;
        }
    }

    public String runtimeState() {
        return lastStatus;
    }

    public String runtimeStatusReason() {
        return lastStatusReason;
    }

    public String dataValidityState() {
        return validityGate.stateId();
    }

    public String dataValidityReason() {
        return validityGate.pauseReason();
    }

    public String dataValidityAge() {
        DataValidityGate.State state = validityGate.state();
        if (state != DataValidityGate.State.PAUSED_AFK && state != DataValidityGate.State.PAUSED_INVALID_WORLD) {
            return "n/a";
        }
        long since = validityGate.stateSinceMs();
        long elapsedMs = Math.max(0L, System.currentTimeMillis() - since);
        return formatDuration(elapsedMs);
    }

    public String dataValidityResumeProgress() {
        long now = System.currentTimeMillis();
        if (validityGate.state() != DataValidityGate.State.RECOVERING) {
            return "n/a";
        }
        long remaining = validityGate.recoveryRemainingMs(now);
        return "movement=" + (validityGate.movementSeenDuringRecovery() ? "yes" : "no")
            + " stable_in=" + formatDuration(remaining);
    }

    public String scalarHintStatus() {
        long now = System.currentTimeMillis();
        GuildSeasonMenuProbe.Observation observation = guildSeasonMenuProbe.latestFreshObservation(now);
        if (observation == null) {
            return "none";
        }
        long ageMs = Math.max(0L, now - observation.observedAtMs());
        return "season=" + observation.seasonId()
            + " territories=" + observation.capturedTerritories()
            + " sr_per_hour=" + observation.srPerHour()
            + " age=" + formatDuration(ageMs);
    }

    public AdvancementTerritoryCollector.DebugLookupResult debugAdvancementTerritory(String query) {
        return collector.debugAdvancementTerritory(query);
    }

    public ScalarDebugSnapshot scalarDebugSnapshot() {
        long now = System.currentTimeMillis();
        GuildSeasonMenuProbe.Observation observation = guildSeasonMenuProbe.latestFreshObservation(now);
        if (observation == null) {
            return ScalarDebugSnapshot.unavailable("no recent scalar hint (open guild manage season status menu)");
        }

        int territories = observation.capturedTerritories();
        int srPerHour = observation.srPerHour();
        double weightedUnits = SeasonScalarMath.weightedUnits(territories);
        double weightedScalar = SeasonScalarMath.scalarWeightedFromSrPerHour(srPerHour, territories);
        double rawScalar = SeasonScalarMath.scalarRawFromSrPerHour(srPerHour, territories);
        long ageMs = Math.max(0L, now - observation.observedAtMs());
        return ScalarDebugSnapshot.available(
            observation.seasonId(),
            territories,
            srPerHour,
            weightedUnits,
            weightedScalar,
            rawScalar,
            formatDuration(ageMs)
        );
    }

    public boolean shareOwner() {
        return config.shareOwner;
    }

    public boolean shareHeadquarters() {
        return config.shareHeadquarters;
    }

    public boolean shareHeldResources() {
        return config.shareHeldResources;
    }

    public boolean shareProductionRates() {
        return config.shareProductionRates;
    }

    public boolean shareStorageCapacity() {
        return config.shareStorageCapacity;
    }

    public boolean shareDefenseTier() {
        return config.shareDefenseTier;
    }

    public boolean shareTradingRoutes() {
        return config.shareTradingRoutes;
    }

    public boolean shareLegacyCaptureSignals() {
        return config.shareLegacyCaptureSignals;
    }

    public boolean shareLegacyWarSignals() {
        return config.shareLegacyWarSignals;
    }

    public void onServerGameMessageSignal(String text, boolean overlay) {
        if (overlay || text == null || text.isBlank()) {
            return;
        }

        long observedAtMs = System.currentTimeMillis();
        LegacyMessageScraper.CaptureSignal capture = LegacyMessageScraper.parseCapture(text, observedAtMs);
        if (capture != null) {
            legacyCaptureSignalsByTerritory.put(capture.territory(), capture);
        }

        LegacyMessageScraper.WarSignal war = LegacyMessageScraper.parseWar(text, observedAtMs);
        if (war != null) {
            legacyWarSignalsByTerritory.put(war.territory(), war);
        }
    }

    public void onTitleSignal(String text) {
        if (!config.enableStrictValidityGate) {
            return;
        }
        validityGate.onTitleText(text, System.currentTimeMillis());
    }

    public void onSubtitleSignal(String text) {
        if (!config.enableStrictValidityGate) {
            return;
        }
        validityGate.onSubtitleText(text, System.currentTimeMillis());
    }

    public void onTitleClearSignal() {
        if (!config.enableStrictValidityGate) {
            return;
        }
        validityGate.onTitleClear(System.currentTimeMillis());
    }

    public void onWorldSignal(String packetType, String details) {
        if (!config.enableStrictValidityGate) {
            return;
        }
        validityGate.onWorldSignal(packetType, details, System.currentTimeMillis());
    }

    public void tick() {
        long now = System.currentTimeMillis();
        pruneLegacySignals(now);
        updateValidityFromClientState(now);
        handleValidityTransitions(now);
        guildSeasonMenuProbe.tick(now);
        maybeStartStartupUpdateCheck();
        pollUpdateCheck(now);
        pollUpdateApply(now);

        pollEnroll(now);
        if (tokenMissingOrExpired() && enrollInFlight == null && now - lastEnrollAttemptMs >= ENROLL_RETRY_MS) {
            lastEnrollAttemptMs = now;
            enrollInFlight = gatewayClient.enrollAsync(config);
            setStatus("enrolling");
        }

        if (validityGate.allowCollection() && now - lastAdvancementScanMs >= ADVANCEMENT_SCAN_MS) {
            lastAdvancementScanMs = now;
            enqueueTerritoryChanges();
        } else if (!validityGate.allowCollection()) {
            // Keep schedule stable and avoid burst scans immediately after resume.
            lastAdvancementScanMs = now;
        }

        maybeCoalesceQueue();

        if (config.token == null || config.token.isBlank()) {
            flushConfigIfDue(now);
            return;
        }

        pollHeartbeat();
        if (heartbeatInFlight == null && now - lastHeartbeatMs >= HEARTBEAT_MS) {
            lastHeartbeatMs = now;
            heartbeatInFlight = gatewayClient.heartbeatAsync(config);
        }
        if (config.token == null || config.token.isBlank()) {
            flushConfigIfDue(now);
            return;
        }

        pollUpload(now);
        if (validityGate.allowDispatch()
            && uploadInFlight == null
            && now - lastUploadDispatchMs >= UPLOAD_DISPATCH_INTERVAL_MS) {
            dispatchUpload(now);
        }

        flushConfigIfDue(now);
    }

    public UpdateCheckStartResult requestUpdateCheck() {
        return startUpdateCheck(new UpdateCheckContext(true));
    }

    public UpdateApplyStartResult requestUpdateApply() {
        if (updateApplyInFlight != null) {
            return UpdateApplyStartResult.ALREADY_RUNNING;
        }
        if (config.autoUpdatePendingAssetUrl == null || config.autoUpdatePendingAssetUrl.isBlank()) {
            return UpdateApplyStartResult.NO_PENDING_UPDATE;
        }
        if (config.autoUpdatePendingAssetSha256 == null || config.autoUpdatePendingAssetSha256.isBlank()) {
            config.autoUpdateLastResult = "update_manifest_asset_hash_missing";
            markConfigDirty(System.currentTimeMillis());
            return UpdateApplyStartResult.INVALID_PENDING_HASH;
        }
        String urlError = IrisAutoUpdater.downloadUrlValidationError(config.autoUpdatePendingAssetUrl);
        if (urlError != null) {
            config.autoUpdateLastResult = urlError;
            markConfigDirty(System.currentTimeMillis());
            return UpdateApplyStartResult.INVALID_ASSET_URL;
        }

        updateApplyTargetVersion = config.autoUpdatePendingVersion;
        updateApplyInFlight = autoUpdater.applyUpdateAsync(
            config.autoUpdatePendingAssetUrl,
            config.autoUpdatePendingAssetSha256,
            config.autoUpdatePendingVersion,
            config.autoUpdateHelperDeadlineMs
        );
        config.autoUpdateLastResult = "applying";
        config.autoUpdateApplyState = "applying";
        markConfigDirty(System.currentTimeMillis());
        return UpdateApplyStartResult.STARTED;
    }

    public UpdateNotification pollUpdateNotification() {
        return updateNotifications.pollFirst();
    }

    private void maybeStartStartupUpdateCheck() {
        if (startupUpdateCheckTriggered) {
            return;
        }
        startupUpdateCheckTriggered = true;
        if (!config.autoUpdateEnabled) {
            return;
        }
        startUpdateCheck(new UpdateCheckContext(false));
    }

    private UpdateCheckStartResult startUpdateCheck(UpdateCheckContext context) {
        if (updateCheckInFlight != null) {
            return UpdateCheckStartResult.ALREADY_RUNNING;
        }

        String normalizedRepo = IrisAutoUpdater.normalizeRepo(config.autoUpdateRepo);
        String repoError = IrisAutoUpdater.repoValidationError(normalizedRepo);
        if (repoError != null) {
            config.autoUpdateLastResult = repoError;
            markConfigDirty(System.currentTimeMillis());
            if (context.manual()) {
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_CHECK_FAILED,
                    RuntimeVersionResolver.currentModVersion(),
                    null,
                    null,
                    repoError
                ));
            }
            return UpdateCheckStartResult.INVALID_REPO;
        }

        config.autoUpdateRepo = normalizedRepo;
        updateCheckContext = context;
        updateCheckInFlight = autoUpdater.checkForUpdateAsync(
            normalizedRepo,
            config.autoUpdateIncludePrerelease,
            RuntimeVersionResolver.currentModVersion(),
            RuntimeVersionResolver.currentMinecraftVersion()
        );
        config.autoUpdateLastResult = "checking";
        markConfigDirty(System.currentTimeMillis());
        return UpdateCheckStartResult.STARTED;
    }

    private void pollUpdateCheck(long now) {
        if (updateCheckInFlight == null || !updateCheckInFlight.isDone()) {
            return;
        }

        IrisAutoUpdater.CheckResult result;
        try {
            result = updateCheckInFlight.getNow(IrisAutoUpdater.CheckResult.failed("update_check_failed"));
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.warn("Updater check task completed exceptionally", e);
            result = IrisAutoUpdater.CheckResult.failed("update_check_failed");
        }
        UpdateCheckContext context = updateCheckContext;
        updateCheckContext = null;
        updateCheckInFlight = null;
        config.autoUpdateLastCheckAt = Instant.now().toString();

        switch (result.status()) {
            case UPDATE_AVAILABLE -> {
                config.autoUpdatePendingVersion = result.latestVersion();
                config.autoUpdatePendingAssetUrl = result.assetUrl();
                config.autoUpdatePendingAssetSha256 = result.assetSha256();
                pendingUpdateReleaseUrl = result.releaseUrl();
                config.autoUpdateLastResult = "update_available";
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_AVAILABLE,
                    result.currentVersion(),
                    result.latestVersion(),
                    result.releaseUrl(),
                    result.reason()
                ));
            }
            case UP_TO_DATE -> {
                clearPendingUpdate();
                config.autoUpdateLastResult = "up_to_date";
                if (context != null && context.manual()) {
                    updateNotifications.addLast(new UpdateNotification(
                        UpdateNotificationKind.UPDATE_UP_TO_DATE,
                        result.currentVersion(),
                        result.latestVersion(),
                        null,
                        result.reason()
                    ));
                }
            }
            case NO_COMPATIBLE_RELEASE -> {
                clearPendingUpdate();
                config.autoUpdateLastResult = result.reason();
                if (context != null && context.manual()) {
                    updateNotifications.addLast(new UpdateNotification(
                        UpdateNotificationKind.UPDATE_NO_COMPATIBLE_RELEASE,
                        result.currentVersion(),
                        result.latestVersion(),
                        null,
                        result.reason()
                    ));
                }
            }
            case FAILED -> {
                config.autoUpdateLastResult = result.reason();
                if (context != null && context.manual()) {
                    updateNotifications.addLast(new UpdateNotification(
                        UpdateNotificationKind.UPDATE_CHECK_FAILED,
                        RuntimeVersionResolver.currentModVersion(),
                        null,
                        null,
                        result.reason()
                    ));
                }
            }
        }

        markConfigDirty(now);
    }

    private void pollUpdateApply(long now) {
        if (updateApplyInFlight == null || !updateApplyInFlight.isDone()) {
            return;
        }

        IrisAutoUpdater.ApplyResult result;
        try {
            result = updateApplyInFlight.getNow(IrisAutoUpdater.ApplyResult.failed("update_apply_failed"));
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.warn("Updater apply task completed exceptionally", e);
            result = IrisAutoUpdater.ApplyResult.failed("update_apply_failed");
        }
        updateApplyInFlight = null;

        switch (result.status()) {
            case STAGED -> {
                config.autoUpdateApplyState = "staged_waiting_for_exit";
                config.autoUpdateJobId = result.jobId();
                config.autoUpdateStagedPath = result.stagedPath();
                config.autoUpdateStagedSha256 = config.autoUpdatePendingAssetSha256;
                config.autoUpdateLastResult = "update_staged";
                config.autoUpdateLastApplyReason = result.reason();
                config.autoUpdateLastApplyAt = Instant.now().toString();
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_APPLY_STAGED,
                    RuntimeVersionResolver.currentModVersion(),
                    updateApplyTargetVersion,
                    null,
                    result.reason()
                ));
            }
            case APPLIED -> {
                String appliedVersion = updateApplyTargetVersion == null || updateApplyTargetVersion.isBlank()
                    ? "unknown"
                    : updateApplyTargetVersion;
                clearPendingUpdate();
                clearApplyJobState();
                config.autoUpdateApplyState = "idle";
                config.autoUpdateLastResult = "apply_success";
                config.autoUpdateLastApplyReason = result.reason();
                config.autoUpdateLastApplyAt = Instant.now().toString();
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_APPLY_SUCCESS,
                    RuntimeVersionResolver.currentModVersion(),
                    appliedVersion,
                    null,
                    result.reason()
                ));
            }
            case FAILED -> {
                config.autoUpdateApplyState = "failed";
                config.autoUpdateLastResult = result.reason();
                config.autoUpdateLastApplyReason = result.reason();
                config.autoUpdateLastApplyAt = Instant.now().toString();
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_APPLY_FAILED,
                    RuntimeVersionResolver.currentModVersion(),
                    updateApplyTargetVersion,
                    null,
                    result.reason()
                ));
            }
        }
        updateApplyTargetVersion = null;
        markConfigDirty(now);
    }

    private void reconcilePendingUpdateJobOnStartup() {
        if (config.autoUpdateJobId == null || config.autoUpdateJobId.isBlank()) {
            return;
        }

        IrisAutoUpdater.ReconcileResult result = autoUpdater.reconcileWindowsJob(config.autoUpdateJobId);
        switch (result.status()) {
            case NONE -> {
                // no-op
            }
            case PENDING -> {
                config.autoUpdateApplyState = "helper_running";
            }
            case SUCCEEDED -> {
                String appliedVersion = config.autoUpdatePendingVersion == null || config.autoUpdatePendingVersion.isBlank()
                    ? "unknown"
                    : config.autoUpdatePendingVersion;
                clearPendingUpdate();
                clearApplyJobState();
                config.autoUpdateApplyState = "idle";
                config.autoUpdateLastResult = "apply_success";
                config.autoUpdateLastApplyReason = result.reason();
                config.autoUpdateLastApplyAt = Instant.now().toString();
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_APPLY_SUCCESS,
                    RuntimeVersionResolver.currentModVersion(),
                    appliedVersion,
                    null,
                    result.reason()
                ));
                markConfigDirty(System.currentTimeMillis());
            }
            case FAILED -> {
                clearApplyJobState();
                config.autoUpdateApplyState = "failed";
                config.autoUpdateLastResult = result.reason();
                config.autoUpdateLastApplyReason = result.reason();
                config.autoUpdateLastApplyAt = Instant.now().toString();
                updateNotifications.addLast(new UpdateNotification(
                    UpdateNotificationKind.UPDATE_APPLY_FAILED,
                    RuntimeVersionResolver.currentModVersion(),
                    config.autoUpdatePendingVersion,
                    null,
                    result.reason()
                ));
                markConfigDirty(System.currentTimeMillis());
            }
        }
    }

    private void clearPendingUpdate() {
        config.autoUpdatePendingVersion = null;
        config.autoUpdatePendingAssetUrl = null;
        config.autoUpdatePendingAssetSha256 = null;
        pendingUpdateReleaseUrl = null;
    }

    private void clearApplyJobState() {
        config.autoUpdateJobId = null;
        config.autoUpdateStagedPath = null;
        config.autoUpdateStagedSha256 = null;
    }

    private void updateValidityFromClientState(long now) {
        if (!config.enableStrictValidityGate) {
            return;
        }

        MinecraftClient client = MinecraftClient.getInstance();
        if (client == null) {
            validityGate.onTickPose(now, false, 0.0, 0.0, 0.0, 0.0f, 0.0f);
            return;
        }

        ClientPlayerEntity player = client.player;
        if (player == null) {
            validityGate.onTickPose(now, false, 0.0, 0.0, 0.0, 0.0f, 0.0f);
            return;
        }

        validityGate.onTickPose(
            now,
            true,
            player.getX(),
            player.getY(),
            player.getZ(),
            player.getYaw(),
            player.getPitch()
        );
    }

    private void handleValidityTransitions(long now) {
        if (!config.enableStrictValidityGate) {
            return;
        }

        DataValidityGate.State current = validityGate.state();
        if (current == lastValidityState) {
            if (current == DataValidityGate.State.RECOVERING && !lastStatus.startsWith("recover")) {
                setStatus("recovering");
            }
            return;
        }

        DataValidityGate.State previous = lastValidityState;
        lastValidityState = current;

        if ((current == DataValidityGate.State.PAUSED_AFK || current == DataValidityGate.State.PAUSED_INVALID_WORLD)
            && previous != DataValidityGate.State.PAUSED_AFK
            && previous != DataValidityGate.State.PAUSED_INVALID_WORLD) {
            clearSubmissionQueueForPause();
            if (current == DataValidityGate.State.PAUSED_AFK) {
                setStatus("paused_afk");
            } else {
                setStatus("paused_invalid_world");
            }
            return;
        }

        if (current == DataValidityGate.State.RECOVERING) {
            setStatus("recovering");
            return;
        }

        if (current == DataValidityGate.State.VALID
            && (previous == DataValidityGate.State.RECOVERING
            || previous == DataValidityGate.State.PAUSED_AFK
            || previous == DataValidityGate.State.PAUSED_INVALID_WORLD)) {
            resetFingerprintCacheForResync();
            setStatus("resyncing");
            // Trigger fresh collection quickly after a successful resume.
            lastAdvancementScanMs = now - ADVANCEMENT_SCAN_MS;
        }
    }

    private void clearSubmissionQueueForPause() {
        queue.clear();
        uploadHeadInFlight = null;
        if (uploadInFlight != null) {
            uploadInFlight.cancel(true);
            uploadInFlight = null;
        }
    }

    private void resetFingerprintCacheForResync() {
        fingerprintByTerritory.clear();
        lastSentAtByTerritory.clear();
    }

    private void pollEnroll(long now) {
        if (enrollInFlight == null || !enrollInFlight.isDone()) {
            return;
        }

        GatewayClient.EnrollResult result;
        try {
            result = enrollInFlight.getNow(GatewayClient.EnrollResult.failed());
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.warn("Enrollment task completed exceptionally", e);
            result = GatewayClient.EnrollResult.failed();
        }
        enrollInFlight = null;
        if (!result.ok || result.token == null || result.token.isBlank()) {
            setStatus("enroll_failed", result.error);
            return;
        }

        config.reporterId = result.reporterId;
        config.token = result.token;
        config.tokenExpiresAt = result.tokenExpiresAt;
        config.guildOptIn = result.guildOptIn;
        GatewayModels.applyTogglesToConfig(config, result.fieldToggles);
        saveConfigNow();
        setStatus("enrolled");
        // Allow immediate heartbeat on fresh enroll.
        lastHeartbeatMs = now - HEARTBEAT_MS;
    }

    private void pollHeartbeat() {
        if (heartbeatInFlight == null || !heartbeatInFlight.isDone()) {
            return;
        }

        GatewayClient.HeartbeatResult result;
        try {
            result = heartbeatInFlight.getNow(GatewayClient.HeartbeatResult.failed());
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.warn("Heartbeat task completed exceptionally", e);
            result = GatewayClient.HeartbeatResult.failed();
        }
        heartbeatInFlight = null;
        if (result.unauthorized) {
            GatewayClient.invalidateToken(config);
            setStatus("heartbeat_reauth", result.error);
            return;
        }
        if (!result.ok) {
            setStatus("heartbeat_retry", result.error);
            return;
        }

        config.tokenExpiresAt = result.tokenExpiresAt;
        config.guildOptIn = result.guildOptIn;
        GatewayModels.applyTogglesToConfig(config, result.fieldToggles);
        if (result.rotatedToken != null && !result.rotatedToken.isBlank()) {
            config.token = result.rotatedToken;
        }
        saveConfigNow();
    }

    private void pollUpload(long now) {
        if (uploadInFlight == null || !uploadInFlight.isDone()) {
            return;
        }

        GatewayClient.SubmitResult result;
        try {
            result = uploadInFlight.getNow(GatewayClient.SubmitResult.failed());
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.warn("Upload task completed exceptionally", e);
            result = GatewayClient.SubmitResult.failed();
        }
        PendingSubmission submitted = uploadHeadInFlight;
        uploadInFlight = null;
        uploadHeadInFlight = null;
        if (submitted == null) {
            return;
        }

        if (result.ok) {
            removeSubmitted(submitted);
            config.lastUploadAt = Instant.now().toString();
            config.lastUploadStatus = result.rejected > 0 ? "partial" : "ok";
            markConfigDirty(now);
            if (result.rejected > 0) {
                setStatus("upload_partial", "accepted=" + result.accepted + " rejected=" + result.rejected);
            } else {
                setStatus("upload_ok");
            }
            return;
        }

        if (result.unauthorized) {
            GatewayClient.invalidateToken(config);
            setStatus("upload_reauth", result.error);
            return;
        }

        if (result.terminal) {
            removeSubmitted(submitted);
            config.lastUploadAt = Instant.now().toString();
            config.lastUploadStatus = "rejected";
            markConfigDirty(now);
            setStatus("upload_rejected", result.error);
            return;
        }

        submitted.attempts += 1;
        long backoffSeconds = Math.min(60, 1L << Math.min(submitted.attempts, 6));
        submitted.nextAttemptMs = now + (backoffSeconds * 1000L);
        config.lastUploadStatus = "retrying";
        markConfigDirty(now);
        setStatus("upload_retry", result.error);
    }

    private void dispatchUpload(long now) {
        if (config.token == null || config.token.isBlank()) {
            return;
        }
        PendingSubmission next = nextDispatchableSubmission(now);
        if (next == null) {
            return;
        }
        lastUploadDispatchMs = now;
        uploadHeadInFlight = next;
        uploadInFlight = gatewayClient.submitTerritoryBatchAsync(config, next.territoryBatch);
    }

    private PendingSubmission nextDispatchableSubmission(long now) {
        for (PendingSubmission pending : queue) {
            if (pending.nextAttemptMs <= now) {
                return pending;
            }
        }
        return null;
    }

    private void removeSubmitted(PendingSubmission submitted) {
        if (queue.peekFirst() == submitted) {
            queue.removeFirst();
        } else {
            queue.remove(submitted);
        }
    }

    private void enqueueTerritoryChanges() {
        long nowMs = System.currentTimeMillis();
        List<GatewayModels.TerritoryUpdate> collected = collector.collect(config);
        Map<String, GatewayModels.TerritoryUpdate> updatesByTerritory = new LinkedHashMap<>();
        for (GatewayModels.TerritoryUpdate update : collected) {
            updatesByTerritory.put(update.territory, update);
        }
        applyLegacySignalsToCollected(updatesByTerritory, nowMs);
        if (updatesByTerritory.isEmpty()) {
            return;
        }

        GatewayModels.TerritoryBatch batch = new GatewayModels.TerritoryBatch();
        String nowIso = Instant.now().toString();
        batch.generated_at = nowIso;
        GuildSeasonMenuProbe.Observation menuObservation =
            guildSeasonMenuProbe.latestFreshObservation(nowMs);
        boolean menuObservationAttached = false;

        for (GatewayModels.TerritoryUpdate update : updatesByTerritory.values()) {
            String fingerprint = fingerprint(update);
            String previous = fingerprintByTerritory.get(update.territory);
            Long lastSentAtMs = lastSentAtByTerritory.get(update.territory);
            boolean refreshDue = lastSentAtMs == null || nowMs - lastSentAtMs >= PERIODIC_REFRESH_MS;
            if (Objects.equals(previous, fingerprint) && !refreshDue) {
                continue;
            }

            fingerprintByTerritory.put(update.territory, fingerprint);
            lastSentAtByTerritory.put(update.territory, nowMs);
            update.idempotency_key = UUID.randomUUID().toString();
            boolean shouldAttachMenuObservation =
                !menuObservationAttached && menuObservation != null && update.runtime != null;
            if (update.runtime != null) {
                update.runtime.provenance = GatewayModels.baseProvenance();
                update.runtime.provenance.put("observed_at", nowIso);
                if (shouldAttachMenuObservation) {
                    menuObservation.attachToProvenance(update.runtime.provenance);
                    menuObservationAttached = true;
                }
            }
            batch.updates.add(update);
        }

        if (batch.updates.isEmpty()) {
            return;
        }

        queue.addLast(new PendingSubmission(batch));
        maybeCoalesceQueue();
    }

    private void applyLegacySignalsToCollected(Map<String, GatewayModels.TerritoryUpdate> updatesByTerritory, long nowMs) {
        pruneLegacySignals(nowMs);

        if (config.shareLegacyCaptureSignals) {
            for (Map.Entry<String, LegacyMessageScraper.CaptureSignal> entry : legacyCaptureSignalsByTerritory.entrySet()) {
                if (!isLegacySignalFresh(entry.getValue().observedAtMs(), nowMs)) {
                    continue;
                }
                GatewayModels.TerritoryUpdate update = updatesByTerritory.computeIfAbsent(
                    entry.getKey(),
                    ReporterRuntime::newTerritoryOnlyUpdate
                );
                Map<String, Object> extraScrapes = ensureRuntimeExtraScrapes(update);
                extraScrapes.put("legacy_capture_signal", serializeCaptureSignal(entry.getValue()));
            }
        }

        if (config.shareLegacyWarSignals) {
            for (Map.Entry<String, LegacyMessageScraper.WarSignal> entry : legacyWarSignalsByTerritory.entrySet()) {
                if (!isLegacySignalFresh(entry.getValue().observedAtMs(), nowMs)) {
                    continue;
                }
                GatewayModels.TerritoryUpdate update = updatesByTerritory.computeIfAbsent(
                    entry.getKey(),
                    ReporterRuntime::newTerritoryOnlyUpdate
                );
                Map<String, Object> extraScrapes = ensureRuntimeExtraScrapes(update);
                extraScrapes.put("legacy_war_signal", serializeWarSignal(entry.getValue()));
            }
        }
    }

    private void pruneLegacySignals(long nowMs) {
        legacyCaptureSignalsByTerritory.values().removeIf(signal ->
            !isLegacySignalFresh(signal.observedAtMs(), nowMs)
        );
        legacyWarSignalsByTerritory.values().removeIf(signal ->
            !isLegacySignalFresh(signal.observedAtMs(), nowMs)
        );
    }

    private static boolean isLegacySignalFresh(long observedAtMs, long nowMs) {
        if (observedAtMs <= 0L) {
            return false;
        }
        return nowMs - observedAtMs <= LEGACY_SIGNAL_TTL_MS;
    }

    private static GatewayModels.TerritoryUpdate newTerritoryOnlyUpdate(String territory) {
        GatewayModels.TerritoryUpdate update = new GatewayModels.TerritoryUpdate();
        update.territory = territory;
        return update;
    }

    private static Map<String, Object> ensureRuntimeExtraScrapes(GatewayModels.TerritoryUpdate update) {
        if (update.runtime == null) {
            update.runtime = new GatewayModels.RuntimeData();
        }
        if (update.runtime.extra_scrapes == null) {
            update.runtime.extra_scrapes = new HashMap<>();
        }
        return update.runtime.extra_scrapes;
    }

    private static Map<String, Object> serializeCaptureSignal(LegacyMessageScraper.CaptureSignal signal) {
        Map<String, Object> out = new HashMap<>();
        out.put("territory", signal.territory());
        out.put("guild_prefix", signal.guildPrefix());
        out.put("observed_at", signal.observedAt());
        out.put("raw_message", signal.rawMessage());
        return out;
    }

    private static Map<String, Object> serializeWarSignal(LegacyMessageScraper.WarSignal signal) {
        Map<String, Object> out = new HashMap<>();
        out.put("territory", signal.territory());
        out.put("kind", signal.kind());
        out.put("observed_at", signal.observedAt());
        out.put("raw_message", signal.rawMessage());
        return out;
    }

    private void maybeCoalesceQueue() {
        if (uploadInFlight != null || queue.size() < QUEUE_COALESCE_THRESHOLD) {
            return;
        }

        Map<String, GatewayModels.TerritoryUpdate> latestByTerritory = new LinkedHashMap<>();
        int mergedAttempts = Integer.MAX_VALUE;
        long mergedNextAttemptMs = Long.MAX_VALUE;
        for (PendingSubmission pending : queue) {
            // Keep the merged batch dispatchable as soon as any source batch is dispatchable.
            mergedAttempts = Math.min(mergedAttempts, pending.attempts);
            mergedNextAttemptMs = Math.min(mergedNextAttemptMs, pending.nextAttemptMs);
            for (GatewayModels.TerritoryUpdate update : pending.territoryBatch.updates) {
                latestByTerritory.put(update.territory, update);
            }
        }

        if (mergedAttempts == Integer.MAX_VALUE) {
            mergedAttempts = 0;
        }
        if (mergedNextAttemptMs == Long.MAX_VALUE) {
            mergedNextAttemptMs = 0L;
        }

        if (latestByTerritory.isEmpty()) {
            return;
        }

        GatewayModels.TerritoryBatch merged = new GatewayModels.TerritoryBatch();
        merged.generated_at = Instant.now().toString();
        for (GatewayModels.TerritoryUpdate update : latestByTerritory.values()) {
            merged.updates.add(update);
        }

        queue.clear();
        queue.addLast(new PendingSubmission(merged, mergedAttempts, mergedNextAttemptMs));
        setStatus("queue_compacted");
    }

    private boolean tokenMissingOrExpired() {
        return isTokenMissingOrExpired(config.token, config.tokenExpiresAt, Instant.now());
    }

    static boolean isTokenMissingOrExpired(String token, String tokenExpiresAt, Instant now) {
        if (token == null || token.isBlank()) {
            return true;
        }
        if (tokenExpiresAt == null || tokenExpiresAt.isBlank()) {
            // Some gateway responses omit an explicit expiry; keep using the token
            // and rely on heartbeat/401 handling rather than re-enrolling in a loop.
            return false;
        }

        try {
            Instant expiresAt = Instant.parse(tokenExpiresAt);
            return expiresAt.isBefore(now.plusSeconds(30));
        } catch (Exception ignored) {
            return true;
        }
    }

    private static String formatDuration(long durationMs) {
        long totalSeconds = Math.max(0L, durationMs / 1000L);
        long minutes = totalSeconds / 60L;
        long seconds = totalSeconds % 60L;
        if (minutes == 0L) {
            return seconds + "s";
        }
        return minutes + "m " + seconds + "s";
    }

    private void setStatus(String status) {
        setStatus(status, null);
    }

    private void setStatus(String status, String reason) {
        lastStatus = status;
        if (reason == null || reason.isBlank()) {
            lastStatusReason = "n/a";
            return;
        }
        lastStatusReason = reason;
    }

    private void markConfigDirty(long now) {
        configDirty = true;
        if (nextConfigFlushMs == 0L) {
            nextConfigFlushMs = now + CONFIG_SAVE_DEBOUNCE_MS;
        }
    }

    private void flushConfigIfDue(long now) {
        if (!configDirty || nextConfigFlushMs == 0L || now < nextConfigFlushMs) {
            return;
        }
        saveConfigNow();
    }

    private void saveConfigNow() {
        ConfigStore.save(config);
        configDirty = false;
        nextConfigFlushMs = 0L;
    }

    private static String fingerprint(GatewayModels.TerritoryUpdate update) {
        // Build a stable fingerprint from payload fields that should trigger transmission.
        Map<String, Object> stable = new HashMap<>();
        stable.put("territory", update.territory);
        stable.put("guild", update.guild);

        if (update.runtime != null) {
            Map<String, Object> runtime = new HashMap<>();
            runtime.put("headquarters", update.runtime.headquarters);
            runtime.put("held_resources", update.runtime.held_resources);
            runtime.put("production_rates", update.runtime.production_rates);
            runtime.put("storage_capacity", update.runtime.storage_capacity);
            runtime.put("defense_tier", update.runtime.defense_tier);
            runtime.put("extra_scrapes", update.runtime.extra_scrapes);
            stable.put("runtime", runtime);
        }

        return GSON.toJson(stable);
    }

    public record ScalarDebugSnapshot(
        boolean available,
        int seasonId,
        int territories,
        int srPerHour,
        double weightedUnits,
        double weightedScalar,
        double rawScalar,
        String age,
        String message
    ) {
        public static ScalarDebugSnapshot unavailable(String message) {
            return new ScalarDebugSnapshot(
                false,
                0,
                0,
                0,
                Double.NaN,
                Double.NaN,
                Double.NaN,
                "n/a",
                message
            );
        }

        public static ScalarDebugSnapshot available(
            int seasonId,
            int territories,
            int srPerHour,
            double weightedUnits,
            double weightedScalar,
            double rawScalar,
            String age
        ) {
            return new ScalarDebugSnapshot(
                true,
                seasonId,
                territories,
                srPerHour,
                weightedUnits,
                weightedScalar,
                rawScalar,
                age,
                ""
            );
        }
    }

    private record UpdateCheckContext(boolean manual) {}

    private static final class PendingSubmission {
        private final GatewayModels.TerritoryBatch territoryBatch;
        private int attempts;
        private long nextAttemptMs;

        private PendingSubmission(GatewayModels.TerritoryBatch territoryBatch) {
            this(territoryBatch, 0, 0L);
        }

        private PendingSubmission(GatewayModels.TerritoryBatch territoryBatch, int attempts, long nextAttemptMs) {
            this.territoryBatch = territoryBatch;
            this.attempts = Math.max(0, attempts);
            this.nextAttemptMs = Math.max(0L, nextAttemptMs);
        }
    }
}
