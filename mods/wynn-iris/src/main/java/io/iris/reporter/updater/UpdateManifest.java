package io.iris.reporter.updater;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Objects;

public final class UpdateManifest {
    public static final String SCHEMA = "iris-update-manifest/v1";
    private static final Gson GSON = new GsonBuilder().create();

    private final String schema;
    private final String releaseTag;
    private final String repo;
    private final String createdAt;
    private final List<Asset> assets;

    private UpdateManifest(String schema, String releaseTag, String repo, String createdAt, List<Asset> assets) {
        this.schema = schema;
        this.releaseTag = releaseTag;
        this.repo = repo;
        this.createdAt = createdAt;
        this.assets = List.copyOf(assets == null ? List.of() : assets);
    }

    public String schema() {
        return schema;
    }

    public String releaseTag() {
        return releaseTag;
    }

    public String repo() {
        return repo;
    }

    public String createdAt() {
        return createdAt;
    }

    public List<Asset> assets() {
        return assets;
    }

    public Asset findAssetByName(String assetName) {
        if (assetName == null || assetName.isBlank()) {
            return null;
        }
        for (Asset asset : assets) {
            if (asset != null && assetName.equals(asset.name())) {
                return asset;
            }
        }
        return null;
    }

    public static UpdateManifest parse(byte[] payload) {
        if (payload == null || payload.length == 0) {
            throw new IllegalArgumentException("manifest_empty");
        }
        JsonElement root = GSON.fromJson(new String(payload, StandardCharsets.UTF_8), JsonElement.class);
        if (root == null || !root.isJsonObject()) {
            throw new IllegalArgumentException("manifest_not_object");
        }

        JsonObject object = root.getAsJsonObject();
        String schema = getString(object, "schema");
        if (!SCHEMA.equals(schema)) {
            throw new IllegalArgumentException("manifest_schema_invalid");
        }

        String releaseTag = getString(object, "release_tag");
        String repo = getString(object, "repo");
        String createdAt = getString(object, "created_at");

        JsonArray assetsArray = getArray(object, "assets");
        if (assetsArray == null || assetsArray.isEmpty()) {
            throw new IllegalArgumentException("manifest_assets_missing");
        }

        List<Asset> assets = new ArrayList<>();
        for (JsonElement element : assetsArray) {
            if (element == null || !element.isJsonObject()) {
                continue;
            }
            JsonObject assetObject = element.getAsJsonObject();
            String name = getString(assetObject, "name");
            String type = getString(assetObject, "type");
            String minecraft = getString(assetObject, "minecraft");
            String version = getString(assetObject, "version");
            String sha256 = normalizeHash(getString(assetObject, "sha256"));
            long size = getLong(assetObject, "size", -1L);
            if (name == null || name.isBlank()) {
                continue;
            }
            assets.add(new Asset(name, type, minecraft, version, sha256, size));
        }

        if (assets.isEmpty()) {
            throw new IllegalArgumentException("manifest_assets_empty");
        }

        return new UpdateManifest(schema, releaseTag, repo, createdAt, assets);
    }

    public static String normalizeHash(String rawHash) {
        if (rawHash == null || rawHash.isBlank()) {
            return null;
        }
        String normalized = rawHash.trim().toLowerCase(Locale.ROOT);
        for (int i = 0; i < normalized.length(); i++) {
            char c = normalized.charAt(i);
            boolean hex = (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
            if (!hex) {
                return null;
            }
        }
        return normalized;
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

    public record Asset(String name, String type, String minecraft, String version, String sha256, long size) {
        public Asset {
            Objects.requireNonNull(name, "name");
        }
    }
}
