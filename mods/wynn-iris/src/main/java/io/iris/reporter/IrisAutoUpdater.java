package io.iris.reporter;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import io.iris.reporter.updater.ApplyResultCode;
import io.iris.reporter.updater.SignatureVerifier;
import io.iris.reporter.updater.UpdateJob;
import io.iris.reporter.updater.UpdateManifest;
import net.fabricmc.loader.api.FabricLoader;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HexFormat;
import java.util.List;
import java.util.Locale;
import java.util.Objects;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.function.BiConsumer;
import java.util.function.Supplier;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.net.URI;
import java.net.URISyntaxException;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;

public final class IrisAutoUpdater {
    static final String DEFAULT_REPO = "OneNoted/sequoia-map";
    private static final Gson GSON = new GsonBuilder().create();
    private static final Set<String> ALLOWED_DOWNLOAD_HOSTS = Set.of(
        "github.com",
        "objects.githubusercontent.com",
        "github-releases.githubusercontent.com"
    );
    private static final Pattern REPO_PATTERN = Pattern.compile("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$");
    private static final Pattern SEMVER_PATTERN =
        Pattern.compile("^(\\d+)\\.(\\d+)\\.(\\d+)(?:-([0-9A-Za-z.-]+))?(?:\\+([0-9A-Za-z._-]+))?$");

    private static final String MANIFEST_ASSET_NAME = "iris-update-manifest.json";
    private static final String MANIFEST_SIGNATURE_ASSET_NAME = "iris-update-manifest.sig";
    private static final String SIGNING_PUBLIC_KEY_BASE64_DER =
        "MCowBQYDK2VwAyEAdkcGNPvN4S2ixBsYjTGJ+4Ue3suPn0Dvo6zuMCTfCCc=";
    private static final long DEFAULT_HELPER_DEADLINE_MS = 20L * 60L * 1000L;

    private final HttpClient httpClient;
    private final ExecutorService requestExecutor;
    private final Supplier<Path> updaterWorkDirSupplier;
    private final HelperLauncher helperLauncher;
    private final SignatureVerifier signatureVerifier;

    public IrisAutoUpdater() {
        this(
            HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(6)).build(),
            Executors.newSingleThreadExecutor(r -> {
                Thread thread = new Thread(r, "wynn-iris-updater");
                thread.setDaemon(true);
                return thread;
            }),
            IrisAutoUpdater::defaultUpdaterWorkDir,
            defaultHelperLauncher(),
            defaultSignatureVerifier()
        );
    }

    IrisAutoUpdater(HttpClient httpClient, ExecutorService requestExecutor) {
        this(httpClient, requestExecutor, IrisAutoUpdater::defaultUpdaterWorkDir, defaultHelperLauncher(), defaultSignatureVerifier());
    }

    IrisAutoUpdater(
        HttpClient httpClient,
        ExecutorService requestExecutor,
        Supplier<Path> updaterWorkDirSupplier,
        HelperLauncher helperLauncher,
        SignatureVerifier signatureVerifier
    ) {
        this.httpClient = Objects.requireNonNull(httpClient, "httpClient");
        this.requestExecutor = Objects.requireNonNull(requestExecutor, "requestExecutor");
        this.updaterWorkDirSupplier = Objects.requireNonNull(updaterWorkDirSupplier, "updaterWorkDirSupplier");
        this.helperLauncher = Objects.requireNonNull(helperLauncher, "helperLauncher");
        this.signatureVerifier = Objects.requireNonNull(signatureVerifier, "signatureVerifier");
    }

    public CompletableFuture<CheckResult> checkForUpdateAsync(
        String repo,
        boolean includePrerelease,
        String currentVersion,
        String minecraftVersion
    ) {
        String normalizedRepo = normalizeRepo(repo);
        String repoError = repoValidationError(normalizedRepo);
        if (repoError != null) {
            return CompletableFuture.completedFuture(CheckResult.failed(repoError));
        }

        SemanticVersion current = parseSemanticVersion(currentVersion);
        if (current == null) {
            return CompletableFuture.completedFuture(CheckResult.failed(ApplyResultCode.CURRENT_VERSION_INVALID.code()));
        }

        String normalizedMinecraftVersion = minecraftVersion == null ? "" : minecraftVersion.trim();
        return CompletableFuture.supplyAsync(
            () -> checkForUpdate(normalizedRepo, includePrerelease, current, normalizedMinecraftVersion),
            requestExecutor
        );
    }

    public CompletableFuture<ApplyResult> applyUpdateAsync(String assetUrl) {
        return applyUpdateAsync(assetUrl, null, null, DEFAULT_HELPER_DEADLINE_MS);
    }

    public CompletableFuture<ApplyResult> applyUpdateAsync(
        String assetUrl,
        String expectedSha256,
        String targetVersion,
        long helperDeadlineMs
    ) {
        return CompletableFuture.supplyAsync(
            () -> applyUpdate(assetUrl, expectedSha256, targetVersion, helperDeadlineMs),
            requestExecutor
        );
    }

    public ReconcileResult reconcileWindowsJob(String jobId) {
        if (!isWindows() || jobId == null || jobId.isBlank()) {
            return ReconcileResult.none();
        }

        Path statusPath = updaterWorkDirSupplier.get().resolve("jobs").resolve(jobId + ".status.json");
        if (!Files.exists(statusPath)) {
            return ReconcileResult.pending(jobId);
        }

        String json;
        try {
            json = Files.readString(statusPath, StandardCharsets.UTF_8);
            Files.deleteIfExists(statusPath);
        } catch (IOException e) {
            return ReconcileResult.failed(jobId, ApplyResultCode.UPDATE_JOB_STATUS_INVALID.code());
        }

        UpdateJob.Status status;
        try {
            status = UpdateJob.statusFromJson(json);
        } catch (RuntimeException e) {
            return ReconcileResult.failed(jobId, ApplyResultCode.UPDATE_JOB_STATUS_INVALID.code());
        }

        if (status.success()) {
            return ReconcileResult.succeeded(jobId, status.reason());
        }
        if (status.failed()) {
            return ReconcileResult.failed(jobId, status.reason());
        }
        return ReconcileResult.failed(jobId, ApplyResultCode.UPDATE_JOB_STATUS_INVALID.code());
    }

    private CheckResult checkForUpdate(
        String repo,
        boolean includePrerelease,
        SemanticVersion currentVersion,
        String minecraftVersion
    ) {
        List<ReleaseSummary> releases;
        try {
            releases = fetchReleases(repo);
        } catch (UpdaterException e) {
            IrisReporterClient.LOGGER.warn("Iris update check failed: {}", e.reason());
            return CheckResult.failed(e.reason());
        }

        List<ReleaseCandidate> candidates = new ArrayList<>();
        SemanticVersion latestSeenVersion = null;
        boolean newerReleaseSeen = false;
        String lastReason = ApplyResultCode.UPDATE_MANIFEST_MISSING.code();

        String marker = "mc" + (minecraftVersion == null ? "" : minecraftVersion.trim());
        for (ReleaseSummary release : releases) {
            if (release == null) {
                continue;
            }
            if (release.prerelease() && !includePrerelease) {
                continue;
            }

            SemanticVersion releaseVersion = parseSemanticVersion(release.tagName());
            if (releaseVersion == null || releaseVersion.compareTo(currentVersion) <= 0) {
                continue;
            }

            newerReleaseSeen = true;
            if (latestSeenVersion == null || releaseVersion.compareTo(latestSeenVersion) > 0) {
                latestSeenVersion = releaseVersion;
            }

            ReleaseAsset jarAsset = findCompatibleAsset(release.assets(), marker);
            if (jarAsset == null) {
                lastReason = "no_compatible_release_asset";
                continue;
            }

            candidates.add(new ReleaseCandidate(release, jarAsset, releaseVersion));
        }

        if (candidates.isEmpty()) {
            if (newerReleaseSeen) {
                String latest = latestSeenVersion == null ? currentVersion.toString() : latestSeenVersion.toString();
                return CheckResult.noCompatibleRelease(currentVersion.toString(), latest, "no_compatible_release_asset");
            }
            return CheckResult.upToDate(currentVersion.toString());
        }

        candidates.sort(Comparator.comparing(ReleaseCandidate::version).reversed());
        for (ReleaseCandidate candidate : candidates) {
            ManifestValidation validation;
            try {
                validation = validateReleaseManifest(
                    repo,
                    candidate.release(),
                    candidate.asset(),
                    candidate.version(),
                    minecraftVersion
                );
            } catch (UpdaterException e) {
                lastReason = e.reason();
                continue;
            }

            return CheckResult.updateAvailable(
                currentVersion.toString(),
                candidate.version().toString(),
                candidate.release().htmlUrl(),
                candidate.asset().downloadUrl(),
                validation.sha256(),
                validation.size()
            );
        }

        String latest = latestSeenVersion == null ? currentVersion.toString() : latestSeenVersion.toString();
        return CheckResult.noCompatibleRelease(currentVersion.toString(), latest, lastReason);
    }

    private ManifestValidation validateReleaseManifest(
        String repo,
        ReleaseSummary release,
        ReleaseAsset jarAsset,
        SemanticVersion releaseVersion,
        String minecraftVersion
    ) throws UpdaterException {
        ReleaseAsset manifestAsset = findAssetByExactName(release.assets(), MANIFEST_ASSET_NAME);
        if (manifestAsset == null) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_MISSING.code());
        }
        ReleaseAsset signatureAsset = findAssetByExactName(release.assets(), MANIFEST_SIGNATURE_ASSET_NAME);
        if (signatureAsset == null) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_SIGNATURE_MISSING.code());
        }

        byte[] manifestPayload;
        byte[] signaturePayload;
        try {
            manifestPayload = downloadPayload(URI.create(manifestAsset.downloadUrl().trim()));
            signaturePayload = downloadPayload(URI.create(signatureAsset.downloadUrl().trim()));
        } catch (IllegalArgumentException e) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_DOWNLOAD_FAILED.code(), e);
        }

        boolean signatureOk;
        try {
            signatureOk = signatureVerifier.verify(manifestPayload, signaturePayload);
        } catch (GeneralSecurityException e) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_SIGNATURE_INVALID.code(), e);
        }
        if (!signatureOk) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_SIGNATURE_INVALID.code());
        }

        UpdateManifest manifest;
        try {
            manifest = UpdateManifest.parse(manifestPayload);
        } catch (RuntimeException e) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_INVALID.code(), e);
        }

        if (manifest.repo() != null && !manifest.repo().isBlank() && !repo.equals(manifest.repo())) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_INVALID.code());
        }
        if (manifest.releaseTag() != null && !manifest.releaseTag().isBlank() && !release.tagName().equals(manifest.releaseTag())) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_INVALID.code());
        }

        UpdateManifest.Asset manifestJar = manifest.findAssetByName(jarAsset.name());
        if (manifestJar == null) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_ASSET_MISSING.code());
        }

        String hash = UpdateManifest.normalizeHash(manifestJar.sha256());
        if (hash == null || hash.length() != 64) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_ASSET_HASH_INVALID.code());
        }

        if (manifestJar.version() != null && !manifestJar.version().isBlank() && !releaseVersion.toString().equals(manifestJar.version())) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_ASSET_VERSION_MISMATCH.code());
        }

        if (manifestJar.minecraft() != null && !manifestJar.minecraft().isBlank()) {
            String expected = minecraftVersion == null ? "" : minecraftVersion.trim();
            if (!expected.equals(manifestJar.minecraft().trim())) {
                throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_ASSET_MINECRAFT_MISMATCH.code());
            }
        }

        long declaredSize = manifestJar.size();
        if (declaredSize > 0 && jarAsset.size() > 0 && declaredSize != jarAsset.size()) {
            throw new UpdaterException(ApplyResultCode.UPDATE_MANIFEST_ASSET_SIZE_INVALID.code());
        }

        return new ManifestValidation(hash, declaredSize);
    }

    private ApplyResult applyUpdate(String assetUrl, String expectedSha256, String targetVersion, long helperDeadlineMs) {
        String validationError = downloadUrlValidationError(assetUrl);
        if (validationError != null) {
            return ApplyResult.failed(validationError);
        }
        String normalizedHash = UpdateManifest.normalizeHash(expectedSha256);
        if (normalizedHash == null || normalizedHash.length() != 64) {
            return ApplyResult.failed(ApplyResultCode.UPDATE_HASH_MISSING.code());
        }

        Path currentJar = resolveCurrentJarPath();
        if (currentJar == null) {
            return ApplyResult.failed(ApplyResultCode.CURRENT_JAR_PATH_UNAVAILABLE.code());
        }

        URI uri = URI.create(assetUrl.trim());
        byte[] payload;
        try {
            payload = downloadJarPayload(uri);
        } catch (UpdaterException e) {
            IrisReporterClient.LOGGER.warn("Iris update download failed: {}", e.reason());
            return ApplyResult.failed(e.reason());
        }

        String payloadHash = sha256Hex(payload);
        if (!normalizedHash.equals(payloadHash)) {
            return ApplyResult.failed(ApplyResultCode.UPDATE_HASH_MISMATCH.code());
        }

        if (!isWindows()) {
            return installDownloadedJar(currentJar, payload);
        }

        try {
            return stageAndLaunchWindowsHelper(currentJar, payload, normalizedHash, targetVersion, helperDeadlineMs);
        } catch (UpdaterException e) {
            return ApplyResult.failed(e.reason());
        }
    }

    private ApplyResult stageAndLaunchWindowsHelper(
        Path currentJar,
        byte[] payload,
        String payloadSha256,
        String targetVersion,
        long helperDeadlineMs
    ) throws UpdaterException {
        if (!Files.exists(currentJar)) {
            throw new UpdaterException(ApplyResultCode.CURRENT_JAR_MISSING.code());
        }
        Path parent = currentJar.getParent();
        if (parent == null) {
            throw new UpdaterException(ApplyResultCode.CURRENT_JAR_PARENT_MISSING.code());
        }

        String fileName = currentJar.getFileName().toString();
        String jobId = UUID.randomUUID().toString();
        Path stagedPath = parent.resolve(fileName + "." + jobId + ".staged");
        Path backupPath = parent.resolve(fileName + ".bak");

        Path updaterDir = updaterWorkDirSupplier.get();
        Path jobsDir = updaterDir.resolve("jobs");
        Path scriptsDir = updaterDir.resolve("scripts");
        Path statusPath = jobsDir.resolve(jobId + ".status.json");
        Path jobPath = jobsDir.resolve(jobId + ".job.json");
        Path helperScriptPath = scriptsDir.resolve("wynn-iris-updater-helper.ps1");

        long now = System.currentTimeMillis();
        long boundedHelperDeadlineMs = Math.max(30_000L, helperDeadlineMs <= 0L ? DEFAULT_HELPER_DEADLINE_MS : helperDeadlineMs);
        long deadlineEpochMs = now + boundedHelperDeadlineMs;

        try {
            Files.createDirectories(parent);
            Files.createDirectories(jobsDir);
            Files.createDirectories(scriptsDir);
            Files.write(stagedPath, payload);
            Files.writeString(helperScriptPath, helperScriptContent(), StandardCharsets.UTF_8);

            UpdateJob job = new UpdateJob(
                jobId,
                ProcessHandle.current().pid(),
                now,
                deadlineEpochMs,
                currentJar,
                stagedPath,
                backupPath,
                statusPath,
                payloadSha256,
                targetVersion == null ? "" : targetVersion
            );
            Files.writeString(jobPath, job.toJson(), StandardCharsets.UTF_8);

            boolean launched = helperLauncher.launch(job, helperScriptPath);
            if (!launched) {
                throw new UpdaterException(ApplyResultCode.UPDATE_HELPER_START_FAILED.code());
            }

            return ApplyResult.staged(stagedPath.toString(), backupPath.toString(), jobId);
        } catch (IOException e) {
            try {
                Files.deleteIfExists(stagedPath);
            } catch (IOException ignored) {
                // Best effort cleanup.
            }
            throw new UpdaterException(ApplyResultCode.UPDATE_HELPER_PREPARE_FAILED.code(), e);
        }
    }

    private List<ReleaseSummary> fetchReleases(String repo) throws UpdaterException {
        URI endpoint = URI.create("https://api.github.com/repos/" + repo + "/releases?per_page=30");
        HttpRequest request = HttpRequest.newBuilder()
            .uri(endpoint)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "wynn-iris-updater")
            .timeout(Duration.ofSeconds(10))
            .GET()
            .build();

        HttpResponse<String> response;
        try {
            response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
        } catch (IOException e) {
            throw new UpdaterException("release_fetch_io", e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new UpdaterException("release_fetch_interrupted", e);
        }

        if (response.statusCode() / 100 != 2) {
            throw new UpdaterException("release_fetch_http_" + response.statusCode());
        }

        JsonElement root;
        try {
            root = GSON.fromJson(response.body(), JsonElement.class);
        } catch (RuntimeException e) {
            throw new UpdaterException("release_json_invalid", e);
        }

        if (root == null || !root.isJsonArray()) {
            throw new UpdaterException("release_json_not_array");
        }

        JsonArray array = root.getAsJsonArray();
        List<ReleaseSummary> releases = new ArrayList<>();
        for (JsonElement element : array) {
            if (element == null || !element.isJsonObject()) {
                continue;
            }
            JsonObject object = element.getAsJsonObject();
            String tagName = getString(object, "tag_name");
            String htmlUrl = getString(object, "html_url");
            boolean prerelease = getBoolean(object, "prerelease", false);

            List<ReleaseAsset> assets = new ArrayList<>();
            JsonArray assetsArray = getArray(object, "assets");
            if (assetsArray != null) {
                for (JsonElement assetElement : assetsArray) {
                    if (assetElement == null || !assetElement.isJsonObject()) {
                        continue;
                    }
                    JsonObject assetObject = assetElement.getAsJsonObject();
                    String name = getString(assetObject, "name");
                    String downloadUrl = getString(assetObject, "browser_download_url");
                    long size = getLong(assetObject, "size", -1L);
                    if (name == null || name.isBlank() || downloadUrl == null || downloadUrl.isBlank()) {
                        continue;
                    }
                    assets.add(new ReleaseAsset(name, downloadUrl, size));
                }
            }

            releases.add(new ReleaseSummary(tagName, prerelease, htmlUrl, assets));
        }

        return releases;
    }

    private byte[] downloadJarPayload(URI originalUri) throws UpdaterException {
        return downloadPayload(originalUri);
    }

    private byte[] downloadPayload(URI originalUri) throws UpdaterException {
        URI current = originalUri;
        for (int redirectCount = 0; redirectCount <= 5; redirectCount++) {
            String uriError = downloadUriValidationError(current);
            if (uriError != null) {
                throw new UpdaterException(uriError);
            }

            HttpRequest request = HttpRequest.newBuilder()
                .uri(current)
                .header("User-Agent", "wynn-iris-updater")
                .timeout(Duration.ofSeconds(20))
                .GET()
                .build();

            HttpResponse<byte[]> response;
            try {
                response = httpClient.send(request, HttpResponse.BodyHandlers.ofByteArray());
            } catch (IOException e) {
                throw new UpdaterException("update_download_io", e);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new UpdaterException("update_download_interrupted", e);
            }

            int status = response.statusCode();
            if (isRedirectStatus(status)) {
                String location = response.headers().firstValue("location").orElse(null);
                if (location == null || location.isBlank()) {
                    throw new UpdaterException("update_download_redirect_missing_location");
                }
                current = resolveRedirectUri(current, location);
                continue;
            }

            if (status / 100 != 2) {
                throw new UpdaterException("update_download_http_" + status);
            }

            byte[] body = response.body();
            if (body == null || body.length == 0) {
                throw new UpdaterException("update_download_empty");
            }
            return body;
        }

        throw new UpdaterException("update_download_too_many_redirects");
    }

    static SelectionResult selectLatestCompatibleRelease(
        List<ReleaseSummary> releases,
        SemanticVersion currentVersion,
        String minecraftVersion,
        boolean includePrerelease
    ) {
        SemanticVersion latestSeenVersion = null;
        SemanticVersion bestCompatibleVersion = null;
        ReleaseSummary bestRelease = null;
        ReleaseAsset bestAsset = null;
        boolean newerReleaseSeen = false;

        String marker = "mc" + (minecraftVersion == null ? "" : minecraftVersion.trim());
        for (ReleaseSummary release : releases) {
            if (release == null) {
                continue;
            }
            if (release.prerelease() && !includePrerelease) {
                continue;
            }

            SemanticVersion releaseVersion = parseSemanticVersion(release.tagName());
            if (releaseVersion == null || releaseVersion.compareTo(currentVersion) <= 0) {
                continue;
            }

            newerReleaseSeen = true;
            if (latestSeenVersion == null || releaseVersion.compareTo(latestSeenVersion) > 0) {
                latestSeenVersion = releaseVersion;
            }

            ReleaseAsset matched = findCompatibleAsset(release.assets(), marker);
            if (matched == null) {
                continue;
            }

            if (bestCompatibleVersion == null || releaseVersion.compareTo(bestCompatibleVersion) > 0) {
                bestCompatibleVersion = releaseVersion;
                bestRelease = release;
                bestAsset = matched;
            }
        }

        return new SelectionResult(bestRelease, bestAsset, bestCompatibleVersion, latestSeenVersion, newerReleaseSeen);
    }

    private static ReleaseAsset findCompatibleAsset(List<ReleaseAsset> assets, String minecraftMarker) {
        if (assets == null || assets.isEmpty()) {
            return null;
        }

        String normalizedMarker = minecraftMarker == null ? "" : minecraftMarker.trim().toLowerCase(Locale.ROOT);
        for (ReleaseAsset asset : assets) {
            if (asset == null) {
                continue;
            }
            String name = asset.name();
            if (name == null) {
                continue;
            }
            String normalized = name.toLowerCase(Locale.ROOT);
            if (!normalized.endsWith(".jar")) {
                continue;
            }
            if (normalized.endsWith("-sources.jar")) {
                continue;
            }
            if (normalizedMarker.isEmpty() || !normalized.contains(normalizedMarker)) {
                continue;
            }
            return asset;
        }
        return null;
    }

    private static ReleaseAsset findAssetByExactName(List<ReleaseAsset> assets, String exactName) {
        if (assets == null || exactName == null || exactName.isBlank()) {
            return null;
        }
        for (ReleaseAsset asset : assets) {
            if (asset != null && exactName.equals(asset.name())) {
                return asset;
            }
        }
        return null;
    }

    public static String normalizeRepo(String repo) {
        if (repo == null || repo.isBlank()) {
            return DEFAULT_REPO;
        }
        return repo.trim();
    }

    public static String repoValidationError(String repo) {
        String normalized = normalizeRepo(repo);
        if (normalized.isBlank()) {
            return ApplyResultCode.UPDATE_REPO_MISSING.code();
        }
        if (!REPO_PATTERN.matcher(normalized).matches()) {
            return ApplyResultCode.UPDATE_REPO_INVALID.code();
        }
        return null;
    }

    public static String downloadUrlValidationError(String downloadUrl) {
        if (downloadUrl == null || downloadUrl.isBlank()) {
            return ApplyResultCode.UPDATE_ASSET_URL_MISSING.code();
        }

        URI uri;
        try {
            uri = URI.create(downloadUrl.trim());
        } catch (IllegalArgumentException e) {
            return ApplyResultCode.UPDATE_ASSET_URL_INVALID.code();
        }

        return downloadUriValidationError(uri);
    }

    static String downloadUriValidationError(URI uri) {
        if (uri == null) {
            return ApplyResultCode.UPDATE_ASSET_URL_INVALID.code();
        }

        String scheme = uri.getScheme();
        if (scheme == null || !scheme.equalsIgnoreCase("https")) {
            return ApplyResultCode.UPDATE_ASSET_URL_INSECURE.code();
        }

        String host = uri.getHost();
        if (host == null || host.isBlank()) {
            return ApplyResultCode.UPDATE_ASSET_URL_MISSING_HOST.code();
        }

        String normalizedHost = host.toLowerCase(Locale.ROOT);
        if (!ALLOWED_DOWNLOAD_HOSTS.contains(normalizedHost)) {
            return ApplyResultCode.UPDATE_ASSET_HOST_NOT_ALLOWED.code();
        }

        return null;
    }

    static SemanticVersion parseSemanticVersion(String rawVersion) {
        if (rawVersion == null || rawVersion.isBlank()) {
            return null;
        }

        String normalized = rawVersion.trim();
        String lower = normalized.toLowerCase(Locale.ROOT);
        if (lower.startsWith("iris-v")) {
            normalized = normalized.substring(6);
        } else if (lower.startsWith("iris-")) {
            normalized = normalized.substring(5);
        }
        if (normalized.startsWith("v") || normalized.startsWith("V")) {
            normalized = normalized.substring(1);
        }

        Matcher matcher = SEMVER_PATTERN.matcher(normalized);
        if (!matcher.matches()) {
            return null;
        }

        int major;
        int minor;
        int patch;
        try {
            major = Integer.parseInt(matcher.group(1));
            minor = Integer.parseInt(matcher.group(2));
            patch = Integer.parseInt(matcher.group(3));
        } catch (NumberFormatException e) {
            return null;
        }

        String prerelease = matcher.group(4);
        String buildMetadata = matcher.group(5);
        List<String> prereleaseIdentifiers = parsePrereleaseIdentifiers(prerelease);

        return new SemanticVersion(major, minor, patch, prereleaseIdentifiers, buildMetadata);
    }

    private static List<String> parsePrereleaseIdentifiers(String prerelease) {
        List<String> identifiers = new ArrayList<>();
        if (prerelease == null || prerelease.isBlank()) {
            return identifiers;
        }

        String[] parts = prerelease.split("\\.");
        for (String part : parts) {
            if (part == null || part.isBlank()) {
                return List.of();
            }
            identifiers.add(part);
        }
        return identifiers;
    }

    static Path resolveCurrentJarPath() {
        try {
            if (IrisReporterClient.class.getProtectionDomain() == null
                || IrisReporterClient.class.getProtectionDomain().getCodeSource() == null
                || IrisReporterClient.class.getProtectionDomain().getCodeSource().getLocation() == null) {
                return null;
            }
            URI location = IrisReporterClient.class.getProtectionDomain().getCodeSource().getLocation().toURI();
            return resolveCurrentJarPathFromLocation(location);
        } catch (URISyntaxException e) {
            return null;
        }
    }

    static Path resolveCurrentJarPathFromLocation(URI location) {
        if (location == null) {
            return null;
        }

        Path path;
        try {
            path = Paths.get(location).normalize();
        } catch (Exception e) {
            return null;
        }

        if (Files.isDirectory(path)) {
            return null;
        }
        Path fileName = path.getFileName();
        if (fileName == null) {
            return null;
        }
        String name = fileName.toString().toLowerCase(Locale.ROOT);
        if (!name.endsWith(".jar")) {
            return null;
        }

        try {
            if (Files.exists(path)) {
                return path.toRealPath();
            }
        } catch (IOException ignored) {
            // Fall back to normalized path.
        }
        return path;
    }

    static ApplyResult installDownloadedJar(Path currentJarPath, byte[] jarPayload) {
        return installDownloadedJar(currentJarPath, jarPayload, IrisAutoUpdater::replaceWithMove);
    }

    static ApplyResult installDownloadedJar(
        Path currentJarPath,
        byte[] jarPayload,
        BiConsumer<Path, Path> replacer
    ) {
        if (currentJarPath == null) {
            return ApplyResult.failed(ApplyResultCode.CURRENT_JAR_PATH_UNAVAILABLE.code());
        }
        if (jarPayload == null || jarPayload.length == 0) {
            return ApplyResult.failed(ApplyResultCode.UPDATE_PAYLOAD_EMPTY.code());
        }
        if (!Files.exists(currentJarPath)) {
            return ApplyResult.failed(ApplyResultCode.CURRENT_JAR_MISSING.code());
        }

        Path parent = currentJarPath.getParent();
        if (parent == null) {
            return ApplyResult.failed(ApplyResultCode.CURRENT_JAR_PARENT_MISSING.code());
        }

        String fileName = currentJarPath.getFileName().toString();
        Path tempPath = parent.resolve(fileName + ".download.tmp");
        Path backupPath = parent.resolve(fileName + ".bak");
        boolean backupCreated = false;

        try {
            Files.createDirectories(parent);
            Files.write(tempPath, jarPayload);

            Files.copy(currentJarPath, backupPath, StandardCopyOption.REPLACE_EXISTING);
            backupCreated = true;

            replacer.accept(tempPath, currentJarPath);

            return ApplyResult.applied(currentJarPath.toString(), backupPath.toString());
        } catch (IOException | RuntimeException e) {
            if (backupCreated) {
                try {
                    Files.copy(backupPath, currentJarPath, StandardCopyOption.REPLACE_EXISTING);
                } catch (IOException restoreError) {
                    IrisReporterClient.LOGGER.warn("Iris updater restore failed", restoreError);
                }
            }
            return ApplyResult.failed(ApplyResultCode.UPDATE_INSTALL_FAILED.code());
        } finally {
            try {
                Files.deleteIfExists(tempPath);
            } catch (IOException ignored) {
                // Best effort cleanup only.
            }
        }
    }

    private static void replaceWithMove(Path source, Path target) {
        try {
            try {
                Files.move(source, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(source, target, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            throw new IllegalStateException(e);
        }
    }

    private static boolean isRedirectStatus(int statusCode) {
        return statusCode == 301 || statusCode == 302 || statusCode == 303 || statusCode == 307 || statusCode == 308;
    }

    private static URI resolveRedirectUri(URI current, String location) throws UpdaterException {
        try {
            URI redirect = URI.create(location);
            if (redirect.isAbsolute()) {
                return redirect;
            }
            return current.resolve(redirect);
        } catch (IllegalArgumentException e) {
            throw new UpdaterException(ApplyResultCode.UPDATE_DOWNLOAD_REDIRECT_INVALID.code(), e);
        }
    }

    private static String getString(JsonObject object, String key) {
        if (object == null || key == null || !object.has(key)) {
            return null;
        }
        JsonElement value = object.get(key);
        if (value == null || value.isJsonNull()) {
            return null;
        }
        try {
            return value.getAsString();
        } catch (RuntimeException e) {
            return null;
        }
    }

    private static boolean getBoolean(JsonObject object, String key, boolean fallback) {
        if (object == null || key == null || !object.has(key)) {
            return fallback;
        }
        JsonElement value = object.get(key);
        if (value == null || value.isJsonNull()) {
            return fallback;
        }
        try {
            return value.getAsBoolean();
        } catch (RuntimeException e) {
            return fallback;
        }
    }

    private static long getLong(JsonObject object, String key, long fallback) {
        if (object == null || key == null || !object.has(key)) {
            return fallback;
        }
        JsonElement value = object.get(key);
        if (value == null || value.isJsonNull()) {
            return fallback;
        }
        try {
            return value.getAsLong();
        } catch (RuntimeException e) {
            return fallback;
        }
    }

    private static JsonArray getArray(JsonObject object, String key) {
        if (object == null || key == null || !object.has(key)) {
            return null;
        }
        JsonElement value = object.get(key);
        if (value == null || !value.isJsonArray()) {
            return null;
        }
        return value.getAsJsonArray();
    }

    private static String sha256Hex(byte[] payload) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            return HexFormat.of().formatHex(digest.digest(payload)).toLowerCase(Locale.ROOT);
        } catch (GeneralSecurityException e) {
            throw new IllegalStateException(e);
        }
    }

    private static boolean isWindows() {
        String os = System.getProperty("os.name", "");
        return os.toLowerCase(Locale.ROOT).contains("win");
    }

    private static Path defaultUpdaterWorkDir() {
        try {
            return FabricLoader.getInstance().getConfigDir().resolve("wynn-iris-updater");
        } catch (Throwable ignored) {
            return Paths.get(System.getProperty("java.io.tmpdir")).resolve("wynn-iris-updater");
        }
    }

    private static SignatureVerifier defaultSignatureVerifier() {
        try {
            return SignatureVerifier.fromBase64DerPublicKey(SIGNING_PUBLIC_KEY_BASE64_DER);
        } catch (GeneralSecurityException e) {
            throw new IllegalStateException("Failed to initialize updater signature verifier", e);
        }
    }

    private static HelperLauncher defaultHelperLauncher() {
        return (job, helperScriptPath) -> {
            List<String> baseArgs = List.of(
                helperScriptPath.toString(),
                job.targetJar().toString(),
                job.stagedJar().toString(),
                job.backupJar().toString(),
                job.expectedSha256(),
                job.statusPath().toString(),
                Long.toString(job.parentPid()),
                Long.toString(job.deadlineEpochMs())
            );

            if (startHelperWithBinary("powershell.exe", baseArgs)) {
                return true;
            }
            return startHelperWithBinary("pwsh.exe", baseArgs);
        };
    }

    private static boolean startHelperWithBinary(String binary, List<String> helperArgs) {
        List<String> command = new ArrayList<>();
        command.add(binary);
        command.add("-NoProfile");
        command.add("-ExecutionPolicy");
        command.add("Bypass");
        command.add("-File");
        command.addAll(helperArgs);

        try {
            new ProcessBuilder(command).start();
            return true;
        } catch (IOException e) {
            return false;
        }
    }

    private static String helperScriptContent() {
        return "param(\n"
            + "  [string]$TargetPath,\n"
            + "  [string]$StagedPath,\n"
            + "  [string]$BackupPath,\n"
            + "  [string]$ExpectedSha256,\n"
            + "  [string]$StatusPath,\n"
            + "  [int64]$ParentPid,\n"
            + "  [int64]$DeadlineEpochMs\n"
            + ")\n"
            + "\n"
            + "function Write-Status([string]$state, [string]$reason) {\n"
            + "  try {\n"
            + "    $payload = @{ state = $state; reason = $reason; finished_at = [DateTimeOffset]::UtcNow.ToString('o') } | ConvertTo-Json -Compress\n"
            + "    [System.IO.File]::WriteAllText($StatusPath, $payload, [System.Text.Encoding]::UTF8)\n"
            + "  } catch { }\n"
            + "}\n"
            + "\n"
            + "function Parent-Alive([int64]$pid) {\n"
            + "  if ($pid -le 0) { return $false }\n"
            + "  try {\n"
            + "    Get-Process -Id $pid -ErrorAction Stop | Out-Null\n"
            + "    return $true\n"
            + "  } catch {\n"
            + "    return $false\n"
            + "  }\n"
            + "}\n"
            + "\n"
            + "$expected = $ExpectedSha256.ToLowerInvariant()\n"
            + "while ((Parent-Alive $ParentPid) -and ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -lt $DeadlineEpochMs)) {\n"
            + "  Start-Sleep -Milliseconds 500\n"
            + "}\n"
            + "\n"
            + "if (Parent-Alive $ParentPid) {\n"
            + "  Write-Status 'failed' 'timeout_waiting_for_exit'\n"
            + "  exit 2\n"
            + "}\n"
            + "\n"
            + "$lastReason = 'update_install_failed'\n"
            + "while ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -lt $DeadlineEpochMs) {\n"
            + "  try {\n"
            + "    if (-not (Test-Path -LiteralPath $StagedPath)) { throw 'staged_missing' }\n"
            + "    if (-not (Test-Path -LiteralPath $TargetPath)) { throw 'target_missing' }\n"
            + "\n"
            + "    Copy-Item -LiteralPath $TargetPath -Destination $BackupPath -Force\n"
            + "    Move-Item -LiteralPath $StagedPath -Destination $TargetPath -Force\n"
            + "\n"
            + "    $actual = (Get-FileHash -LiteralPath $TargetPath -Algorithm SHA256).Hash.ToLowerInvariant()\n"
            + "    if ($actual -ne $expected) { throw 'update_hash_mismatch' }\n"
            + "\n"
            + "    Write-Status 'success' 'applied'\n"
            + "    exit 0\n"
            + "  } catch {\n"
            + "    $lastReason = $_.Exception.Message\n"
            + "    try {\n"
            + "      if (Test-Path -LiteralPath $BackupPath) {\n"
            + "        Copy-Item -LiteralPath $BackupPath -Destination $TargetPath -Force\n"
            + "      }\n"
            + "    } catch { }\n"
            + "    Start-Sleep -Milliseconds 750\n"
            + "  }\n"
            + "}\n"
            + "\n"
            + "Write-Status 'failed' $lastReason\n"
            + "exit 3\n";
    }

    public enum CheckStatus {
        UP_TO_DATE,
        UPDATE_AVAILABLE,
        NO_COMPATIBLE_RELEASE,
        FAILED
    }

    public record CheckResult(
        CheckStatus status,
        String currentVersion,
        String latestVersion,
        String releaseUrl,
        String assetUrl,
        String assetSha256,
        long assetSize,
        String reason
    ) {
        static CheckResult upToDate(String currentVersion) {
            return new CheckResult(CheckStatus.UP_TO_DATE, currentVersion, currentVersion, null, null, null, -1L, "up_to_date");
        }

        static CheckResult updateAvailable(
            String currentVersion,
            String latestVersion,
            String releaseUrl,
            String assetUrl,
            String assetSha256,
            long assetSize
        ) {
            return new CheckResult(
                CheckStatus.UPDATE_AVAILABLE,
                currentVersion,
                latestVersion,
                releaseUrl,
                assetUrl,
                assetSha256,
                assetSize,
                "update_available"
            );
        }

        static CheckResult noCompatibleRelease(String currentVersion, String latestVersion, String reason) {
            return new CheckResult(CheckStatus.NO_COMPATIBLE_RELEASE, currentVersion, latestVersion, null, null, null, -1L, reason);
        }

        static CheckResult failed(String reason) {
            return new CheckResult(CheckStatus.FAILED, null, null, null, null, null, -1L, reason);
        }
    }

    public enum ApplyStatus {
        STAGED,
        APPLIED,
        FAILED
    }

    public record ApplyResult(
        ApplyStatus status,
        String installedPath,
        String backupPath,
        String stagedPath,
        String jobId,
        String reason
    ) {
        static ApplyResult staged(String stagedPath, String backupPath, String jobId) {
            return new ApplyResult(ApplyStatus.STAGED, null, backupPath, stagedPath, jobId, ApplyResultCode.STAGED.code());
        }

        static ApplyResult applied(String installedPath, String backupPath) {
            return new ApplyResult(ApplyStatus.APPLIED, installedPath, backupPath, null, null, ApplyResultCode.APPLIED.code());
        }

        static ApplyResult failed(String reason) {
            String normalized = reason == null || reason.isBlank() ? ApplyResultCode.FAILED.code() : reason;
            return new ApplyResult(ApplyStatus.FAILED, null, null, null, null, normalized);
        }
    }

    public enum ReconcileStatus {
        NONE,
        PENDING,
        SUCCEEDED,
        FAILED
    }

    public record ReconcileResult(ReconcileStatus status, String jobId, String reason) {
        static ReconcileResult none() {
            return new ReconcileResult(ReconcileStatus.NONE, null, null);
        }

        static ReconcileResult pending(String jobId) {
            return new ReconcileResult(ReconcileStatus.PENDING, jobId, ApplyResultCode.UPDATE_JOB_STATUS_MISSING.code());
        }

        static ReconcileResult succeeded(String jobId, String reason) {
            return new ReconcileResult(ReconcileStatus.SUCCEEDED, jobId, reason == null || reason.isBlank() ? ApplyResultCode.APPLIED.code() : reason);
        }

        static ReconcileResult failed(String jobId, String reason) {
            return new ReconcileResult(ReconcileStatus.FAILED, jobId, reason == null || reason.isBlank() ? ApplyResultCode.UPDATE_APPLY_FAILED.code() : reason);
        }
    }

    static record ReleaseSummary(String tagName, boolean prerelease, String htmlUrl, List<ReleaseAsset> assets) {}

    static record ReleaseAsset(String name, String downloadUrl, long size) {
        ReleaseAsset(String name, String downloadUrl) {
            this(name, downloadUrl, -1L);
        }
    }

    static record SelectionResult(
        ReleaseSummary release,
        ReleaseAsset asset,
        SemanticVersion version,
        SemanticVersion latestSeenVersion,
        boolean newerReleaseSeen
    ) {}

    private record ReleaseCandidate(ReleaseSummary release, ReleaseAsset asset, SemanticVersion version) {}

    private record ManifestValidation(String sha256, long size) {}

    @FunctionalInterface
    interface HelperLauncher {
        boolean launch(UpdateJob job, Path helperScriptPath) throws IOException;
    }

    static final class SemanticVersion implements Comparable<SemanticVersion> {
        private final int major;
        private final int minor;
        private final int patch;
        private final List<String> prerelease;
        private final String buildMetadata;

        private SemanticVersion(int major, int minor, int patch, List<String> prerelease, String buildMetadata) {
            this.major = major;
            this.minor = minor;
            this.patch = patch;
            this.prerelease = List.copyOf(prerelease == null ? List.of() : prerelease);
            this.buildMetadata = buildMetadata == null ? "" : buildMetadata;
        }

        @Override
        public int compareTo(SemanticVersion other) {
            if (other == null) {
                return 1;
            }

            int cmp = Integer.compare(major, other.major);
            if (cmp != 0) {
                return cmp;
            }
            cmp = Integer.compare(minor, other.minor);
            if (cmp != 0) {
                return cmp;
            }
            cmp = Integer.compare(patch, other.patch);
            if (cmp != 0) {
                return cmp;
            }

            boolean thisHasPrerelease = !prerelease.isEmpty();
            boolean otherHasPrerelease = !other.prerelease.isEmpty();
            if (!thisHasPrerelease && !otherHasPrerelease) {
                return 0;
            }
            if (!thisHasPrerelease) {
                return 1;
            }
            if (!otherHasPrerelease) {
                return -1;
            }

            int len = Math.max(prerelease.size(), other.prerelease.size());
            for (int idx = 0; idx < len; idx++) {
                if (idx >= prerelease.size()) {
                    return -1;
                }
                if (idx >= other.prerelease.size()) {
                    return 1;
                }

                String left = prerelease.get(idx);
                String right = other.prerelease.get(idx);
                boolean leftNumeric = isNumeric(left);
                boolean rightNumeric = isNumeric(right);

                if (leftNumeric && rightNumeric) {
                    cmp = Integer.compare(Integer.parseInt(left), Integer.parseInt(right));
                } else if (leftNumeric) {
                    cmp = -1;
                } else if (rightNumeric) {
                    cmp = 1;
                } else {
                    cmp = left.compareTo(right);
                }

                if (cmp != 0) {
                    return cmp;
                }
            }

            return 0;
        }

        private static boolean isNumeric(String value) {
            if (value == null || value.isEmpty()) {
                return false;
            }
            for (int idx = 0; idx < value.length(); idx++) {
                if (!Character.isDigit(value.charAt(idx))) {
                    return false;
                }
            }
            return true;
        }

        @Override
        public String toString() {
            StringBuilder out = new StringBuilder();
            out.append(major).append('.').append(minor).append('.').append(patch);
            if (!prerelease.isEmpty()) {
                out.append('-').append(String.join(".", prerelease));
            }
            if (!buildMetadata.isBlank()) {
                out.append('+').append(buildMetadata);
            }
            return out.toString();
        }
    }

    private static final class UpdaterException extends Exception {
        private final String reason;

        private UpdaterException(String reason) {
            super(reason);
            this.reason = reason;
        }

        private UpdaterException(String reason, Throwable cause) {
            super(reason, cause);
            this.reason = reason;
        }

        String reason() {
            return reason;
        }
    }
}
