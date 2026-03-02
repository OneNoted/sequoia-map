package io.iris.reporter;

public final class ReporterConfig {
    public String ingestBaseUrl = "https://seqwawa.com";
    public boolean allowInsecureIngestHttp = false;
    public String reporterId;
    public String token;
    public String tokenExpiresAt;

    // Backward-compatible no-op field accepted by ingest for one phase.
    public boolean guildOptIn = false;

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
