package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayDeque;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;

public class ReporterRuntimeQueueSchedulingTest {
    @Test
    void nextDispatchableSubmissionSkipsBackedOffHead() throws Exception {
        ReporterRuntime runtime = new ReporterRuntime(new ReporterConfig());
        ArrayDeque<Object> queue = queue(runtime);

        Object blocked = newPending(newBatch("A"), 3, 9_000L);
        Object ready = newPending(newBatch("B"), 0, 0L);
        queue.addLast(blocked);
        queue.addLast(ready);

        Object selected = invoke(runtime, "nextDispatchableSubmission", 1_000L);
        assertSame(ready, selected);
    }

    @Test
    void queueCoalesceKeepsEarliestRetryMetadata() throws Exception {
        ReporterRuntime runtime = new ReporterRuntime(new ReporterConfig());
        ArrayDeque<Object> queue = queue(runtime);

        queue.addLast(newPending(newBatch("A"), 4, 7_000L));
        queue.addLast(newPending(newBatch("B"), 0, 0L));
        queue.addLast(newPending(newBatch("C"), 2, 3_000L));
        queue.addLast(newPending(newBatch("D"), 1, 1_000L));

        invoke(runtime, "maybeCoalesceQueue");

        assertEquals(1, queue.size());
        Object merged = queue.peekFirst();
        assertEquals(0, intField(merged, "attempts"));
        assertEquals(0L, longField(merged, "nextAttemptMs"));
    }

    @SuppressWarnings("unchecked")
    private static ArrayDeque<Object> queue(ReporterRuntime runtime) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField("queue");
        field.setAccessible(true);
        return (ArrayDeque<Object>) field.get(runtime);
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

    private static Object invoke(ReporterRuntime runtime, String methodName, Object... args) throws Exception {
        Class<?>[] parameterTypes = new Class<?>[args.length];
        for (int i = 0; i < args.length; i++) {
            parameterTypes[i] = args[i] instanceof Long ? long.class : args[i].getClass();
        }
        Method method = ReporterRuntime.class.getDeclaredMethod(methodName, parameterTypes);
        method.setAccessible(true);
        return method.invoke(runtime, args);
    }

    private static int intField(Object target, String fieldName) throws Exception {
        Field field = target.getClass().getDeclaredField(fieldName);
        field.setAccessible(true);
        return field.getInt(target);
    }

    private static long longField(Object target, String fieldName) throws Exception {
        Field field = target.getClass().getDeclaredField(fieldName);
        field.setAccessible(true);
        return field.getLong(target);
    }
}
