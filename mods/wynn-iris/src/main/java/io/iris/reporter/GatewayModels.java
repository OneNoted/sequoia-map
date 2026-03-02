package io.iris.reporter;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

public final class GatewayModels {
    private GatewayModels() {}

    public static final class FieldToggles {
        public boolean share_owner = true;
        public boolean share_headquarters = true;
        public boolean share_held_resources = true;
        public boolean share_production_rates = true;
        public boolean share_storage_capacity = true;
        public boolean share_defense_tier = true;
        public boolean share_trading_routes = false;
    }

    public static final class EnrollRequest {
        public String reporter_id;
        public boolean guild_opt_in;
        public String minecraft_version;
        public String mod_version;
        public FieldToggles field_toggles;
    }

    public static final class EnrollResponse {
        public boolean ok;
        public String reporter_id;
        public String token;
        public String token_expires_at;
        public boolean guild_opt_in;
        public FieldToggles field_toggles;
    }

    public static final class HeartbeatRequest {
        public Boolean guild_opt_in;
        public FieldToggles field_toggles;
    }

    public static final class HeartbeatResponse {
        public boolean ok;
        public String reporter_id;
        public String token_expires_at;
        public String rotated_token;
        public boolean guild_opt_in;
        public FieldToggles field_toggles;
    }

    public static final class TerritoryBatch {
        public String generated_at;
        public List<TerritoryUpdate> updates = new ArrayList<>();
    }

    public static final class TerritoryUpdate {
        public String territory;
        public Map<String, Object> guild;
        public String acquired;
        public List<String> connections;
        public RuntimeData runtime;
        public String idempotency_key;
    }

    public static final class RuntimeData {
        public Boolean headquarters;
        public Resources held_resources;
        public Resources production_rates;
        public Resources storage_capacity;
        public String defense_tier;
        public Map<String, Object> extra_scrapes;
        public Map<String, Object> provenance;
    }

    public static final class Resources {
        public int emeralds;
        public int ore;
        public int crops;
        public int fish;
        public int wood;

        public static Resources of(int emeralds, int ore, int crops, int fish, int wood) {
            Resources resources = new Resources();
            resources.emeralds = emeralds;
            resources.ore = ore;
            resources.crops = crops;
            resources.fish = fish;
            resources.wood = wood;
            return resources;
        }

        public boolean isEmpty() {
            return emeralds == 0 && ore == 0 && crops == 0 && fish == 0 && wood == 0;
        }

        public Resources copy() {
            return of(emeralds, ore, crops, fish, wood);
        }
    }

    public static FieldToggles fromConfig(ReporterConfig config) {
        FieldToggles toggles = new FieldToggles();
        toggles.share_owner = config.shareOwner;
        toggles.share_headquarters = config.shareHeadquarters;
        toggles.share_held_resources = config.shareHeldResources;
        toggles.share_production_rates = config.shareProductionRates;
        toggles.share_storage_capacity = config.shareStorageCapacity;
        toggles.share_defense_tier = config.shareDefenseTier;
        toggles.share_trading_routes = config.shareTradingRoutes;
        return toggles;
    }

    public static void applyTogglesToConfig(ReporterConfig config, FieldToggles toggles) {
        if (toggles == null) {
            return;
        }
        config.shareOwner = toggles.share_owner;
        config.shareHeadquarters = toggles.share_headquarters;
        config.shareHeldResources = toggles.share_held_resources;
        config.shareProductionRates = toggles.share_production_rates;
        config.shareStorageCapacity = toggles.share_storage_capacity;
        config.shareDefenseTier = toggles.share_defense_tier;
        config.shareTradingRoutes = toggles.share_trading_routes;
    }

    public static Map<String, Object> baseProvenance() {
        Map<String, Object> provenance = new HashMap<>();
        provenance.put("source", "fabric_reporter");
        provenance.put("visibility", "public");
        provenance.put("confidence", 0.6);
        provenance.put("reporter_count", 1);
        return provenance;
    }
}
