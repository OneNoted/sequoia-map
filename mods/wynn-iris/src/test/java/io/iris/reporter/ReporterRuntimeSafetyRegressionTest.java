package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayDeque;
import java.util.LinkedHashMap;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

public class ReporterRuntimeSafetyRegressionTest {
    @Test
    void dispatchUploadSkipsWhenTokenIsMissing() throws Exception {
        ReporterConfig config = new ReporterConfig();
        config.token = null;
        ReporterRuntime runtime = new ReporterRuntime(config);
        queue(runtime).addLast(newPending(newBatch("Nivla Woods"), 0, 0L));

        invoke(runtime, "dispatchUpload", 1_000L);

        assertNull(getField(runtime, "uploadInFlight"));
        assertTrue(queue(runtime).size() == 1);
    }

    @Test
    void staleLegacySignalsArePrunedBeforeApplyingToUpdates() throws Exception {
        ReporterConfig config = new ReporterConfig();
        config.shareLegacyCaptureSignals = true;
        ReporterRuntime runtime = new ReporterRuntime(config);

        long now = System.currentTimeMillis();
        LegacyMessageScraper.CaptureSignal stale = LegacyMessageScraper.parseCapture(
            "Old Territory was captured by [OLD]",
            now - 600_000L
        );
        LegacyMessageScraper.CaptureSignal fresh = LegacyMessageScraper.parseCapture(
            "Fresh Territory was captured by [NEW]",
            now - 1_000L
        );
        assertNotNull(stale);
        assertNotNull(fresh);

        Map<String, LegacyMessageScraper.CaptureSignal> captures = captureSignals(runtime);
        captures.put(stale.territory(), stale);
        captures.put(fresh.territory(), fresh);

        Map<String, GatewayModels.TerritoryUpdate> updates = new LinkedHashMap<>();
        invoke(runtime, "applyLegacySignalsToCollected", updates, now);

        assertFalse(updates.containsKey(stale.territory()));
        assertTrue(updates.containsKey(fresh.territory()));
        assertFalse(captures.containsKey(stale.territory()));
        assertTrue(captures.containsKey(fresh.territory()));
    }

    private static GatewayModels.TerritoryBatch newBatch(String territory) {
        GatewayModels.TerritoryBatch batch = new GatewayModels.TerritoryBatch();
        GatewayModels.TerritoryUpdate update = new GatewayModels.TerritoryUpdate();
        update.territory = territory;
        batch.updates.add(update);
        return batch;
    }

    private static Object newPending(GatewayModels.TerritoryBatch batch, int attempts, long nextAttemptMs) throws Exception {
        Class<?> pendingClass = Class.forName("io.iris.reporter.ReporterRuntime$PendingSubmission");
        Constructor<?> constructor = pendingClass.getDeclaredConstructor(GatewayModels.TerritoryBatch.class, int.class, long.class);
        constructor.setAccessible(true);
        return constructor.newInstance(batch, attempts, nextAttemptMs);
    }

    @SuppressWarnings("unchecked")
    private static ArrayDeque<Object> queue(ReporterRuntime runtime) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField("queue");
        field.setAccessible(true);
        return (ArrayDeque<Object>) field.get(runtime);
    }

    @SuppressWarnings("unchecked")
    private static Map<String, LegacyMessageScraper.CaptureSignal> captureSignals(ReporterRuntime runtime) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField("legacyCaptureSignalsByTerritory");
        field.setAccessible(true);
        return (Map<String, LegacyMessageScraper.CaptureSignal>) field.get(runtime);
    }

    private static Object invoke(ReporterRuntime runtime, String methodName, Object... args) throws Exception {
        Class<?>[] parameterTypes = new Class<?>[args.length];
        for (int idx = 0; idx < args.length; idx++) {
            if (args[idx] instanceof Long) {
                parameterTypes[idx] = long.class;
            } else if (args[idx] instanceof Map<?, ?>) {
                parameterTypes[idx] = Map.class;
            } else {
                parameterTypes[idx] = args[idx].getClass();
            }
        }
        Method method = ReporterRuntime.class.getDeclaredMethod(methodName, parameterTypes);
        method.setAccessible(true);
        return method.invoke(runtime, args);
    }

    private static Object getField(ReporterRuntime runtime, String fieldName) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField(fieldName);
        field.setAccessible(true);
        return field.get(runtime);
    }
}
