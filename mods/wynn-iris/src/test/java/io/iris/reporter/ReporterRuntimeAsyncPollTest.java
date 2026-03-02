package io.iris.reporter;

import org.junit.jupiter.api.Test;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.concurrent.CompletableFuture;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertNull;

public class ReporterRuntimeAsyncPollTest {
    @Test
    void pollEnrollHandlesExceptionalFuture() throws Exception {
        ReporterRuntime runtime = new ReporterRuntime(new ReporterConfig());
        setField(runtime, "enrollInFlight", CompletableFuture.failedFuture(new RuntimeException("boom")));

        assertDoesNotThrow(() -> invoke(runtime, "pollEnroll", 1_000L));
        assertNull(getField(runtime, "enrollInFlight"));
    }

    @Test
    void pollHeartbeatHandlesExceptionalFuture() throws Exception {
        ReporterRuntime runtime = new ReporterRuntime(new ReporterConfig());
        setField(runtime, "heartbeatInFlight", CompletableFuture.failedFuture(new RuntimeException("boom")));

        assertDoesNotThrow(() -> invoke(runtime, "pollHeartbeat"));
        assertNull(getField(runtime, "heartbeatInFlight"));
    }

    @Test
    void pollUploadHandlesExceptionalFuture() throws Exception {
        ReporterRuntime runtime = new ReporterRuntime(new ReporterConfig());
        Object pending = newPendingSubmission();
        setField(runtime, "uploadHeadInFlight", pending);
        setField(runtime, "uploadInFlight", CompletableFuture.failedFuture(new RuntimeException("boom")));

        assertDoesNotThrow(() -> invoke(runtime, "pollUpload", 2_000L));
        assertNull(getField(runtime, "uploadInFlight"));
        assertNull(getField(runtime, "uploadHeadInFlight"));
    }

    private static Object newPendingSubmission() throws Exception {
        Class<?> pendingClass = Class.forName("io.iris.reporter.ReporterRuntime$PendingSubmission");
        Constructor<?> constructor = pendingClass.getDeclaredConstructor(GatewayModels.TerritoryBatch.class);
        constructor.setAccessible(true);
        return constructor.newInstance(new GatewayModels.TerritoryBatch());
    }

    private static void invoke(ReporterRuntime runtime, String methodName, Object... args) throws Exception {
        Class<?>[] parameterTypes = new Class<?>[args.length];
        for (int i = 0; i < args.length; i++) {
            parameterTypes[i] = args[i] instanceof Long ? long.class : args[i].getClass();
        }
        Method method = ReporterRuntime.class.getDeclaredMethod(methodName, parameterTypes);
        method.setAccessible(true);
        method.invoke(runtime, args);
    }

    private static void setField(ReporterRuntime runtime, String fieldName, Object value) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField(fieldName);
        field.setAccessible(true);
        field.set(runtime, value);
    }

    private static Object getField(ReporterRuntime runtime, String fieldName) throws Exception {
        Field field = ReporterRuntime.class.getDeclaredField(fieldName);
        field.setAccessible(true);
        return field.get(runtime);
    }
}
