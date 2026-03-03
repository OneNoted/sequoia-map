package io.iris.reporter;

public final class ReporterConfig {
    public String ingestBaseUrl = "https://map.seqwawa.com";
    public boolean allowInsecureIngestHttp = false;
    public String reporterId;
    public String token;
    public String tokenExpiresAt;
    public String devicePrivateKeyB64;
    public String devicePublicKeyB64;
    public String deviceKeyId;

    // Backward-compatible no-op field accepted by ingest for one phase.
    public boolean guildOptIn = false;

    public boolean autoUpdateEnabled = true;
    public String autoUpdateRepo = IrisAutoUpdater.DEFAULT_REPO;
    public boolean autoUpdateIncludePrerelease = false;
    public String autoUpdateLastCheckAt;
    public String autoUpdateLastResult = "never";
    public String autoUpdatePendingVersion;
    public String autoUpdatePendingAssetUrl;
    public String autoUpdatePendingAssetSha256;
    public String autoUpdateApplyState = "idle";
    public String autoUpdateJobId;
    public String autoUpdateStagedPath;
    public String autoUpdateStagedSha256;
    public String autoUpdateLastApplyReason = "never";
    public String autoUpdateLastApplyAt;
    public long autoUpdateHelperDeadlineMs = 1_200_000L;

    public boolean shareOwner = true;
    public boolean shareHeadquarters = true;
    public boolean shareHeldResources = true;
    public boolean shareProductionRates = true;
    public boolean shareStorageCapacity = true;
    public boolean shareDefenseTier = true;
    public boolean shareTradingRoutes = false;
    public boolean shareLegacyCaptureSignals = false;
    public boolean shareLegacyWarSignals = false;

    public boolean enableStrictValidityGate = true;
    public long resumeStabilizationMs = 10_000;
    public double minMovementBlocks = 0.25;

    public String lastUploadStatus = "never";
    public String lastUploadAt;
}
