package io.iris.reporter;

import net.minecraft.text.Text;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

import java.lang.reflect.Field;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;

public class PacketSignalBridgeTest {
    @AfterEach
    void clearRuntime() throws Exception {
        setRuntime(null);
    }

    @Test
    void forwardsSignalsToRuntimeValidityGate() throws Exception {
        ReporterConfig config = new ReporterConfig();
        config.enableStrictValidityGate = true;
        ReporterRuntime runtime = new ReporterRuntime(config);
        setRuntime(runtime);

        PacketSignalBridge.onSubtitle(Text.literal("Move to continue"));
        assertEquals("paused_afk", runtime.dataValidityState());

        PacketSignalBridge.onTitleClear();
        assertEquals("title_cleared_waiting_world", runtime.dataValidityReason());

        PacketSignalBridge.onWorldEvent("GameJoinS2CPacket", "minecraft:overworld");
        assertEquals("recovering", runtime.dataValidityState());

        PacketSignalBridge.onTitle(null);
        assertNotNull(runtime.dataValidityState());
    }

    private static void setRuntime(ReporterRuntime runtime) throws Exception {
        Field field = IrisReporterClient.class.getDeclaredField("runtime");
        field.setAccessible(true);
        field.set(null, runtime);
    }
}
