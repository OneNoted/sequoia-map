package io.iris.reporter;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonSyntaxException;
import net.fabricmc.loader.api.FabricLoader;

import java.io.IOException;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;

public final class ConfigStore {
    private static final Gson GSON = new GsonBuilder().setPrettyPrinting().create();
    private static final String CONFIG_FILE_NAME = "wynn-iris.json";

    private ConfigStore() {}

    public static ReporterConfig load() {
        Path path = configPath();
        if (path == null) {
            return new ReporterConfig();
        }
        if (!Files.exists(path)) {
            ReporterConfig defaults = new ReporterConfig();
            save(defaults);
            return defaults;
        }

        ReporterConfig config = readConfig(path);
        if (config == null) {
            return new ReporterConfig();
        }

        return config;
    }

    private static ReporterConfig readConfig(Path path) {
        try {
            String json = Files.readString(path, StandardCharsets.UTF_8);
            ReporterConfig config = GSON.fromJson(json, ReporterConfig.class);
            if (config == null) {
                config = new ReporterConfig();
            }
            return config;
        } catch (IOException | JsonSyntaxException e) {
            IrisReporterClient.LOGGER.warn("Failed to load reporter config from {}", path, e);
            return null;
        }
    }

    public static void save(ReporterConfig config) {
        Path path = configPath();
        if (path == null) {
            return;
        }
        Path tempPath = path.resolveSibling(path.getFileName().toString() + ".tmp");
        try {
            Files.createDirectories(path.getParent());
            Files.writeString(
                tempPath,
                GSON.toJson(config),
                StandardCharsets.UTF_8,
                StandardOpenOption.CREATE,
                StandardOpenOption.TRUNCATE_EXISTING,
                StandardOpenOption.WRITE
            );
            try {
                Files.move(tempPath, path, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(tempPath, path, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            IrisReporterClient.LOGGER.warn("Failed to save reporter config", e);
            try {
                Files.deleteIfExists(tempPath);
            } catch (IOException ignored) {
                // Best effort cleanup only.
            }
        }
    }

    private static Path configPath() {
        try {
            Path configDir = FabricLoader.getInstance().getConfigDir();
            if (configDir == null) {
                return null;
            }
            return configDir.resolve(CONFIG_FILE_NAME);
        } catch (RuntimeException e) {
            IrisReporterClient.LOGGER.debug("Config path unavailable in current runtime", e);
            return null;
        }
    }
}
