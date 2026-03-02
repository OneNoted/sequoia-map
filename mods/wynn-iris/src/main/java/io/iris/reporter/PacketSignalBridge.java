package io.iris.reporter;

import net.minecraft.text.Text;

public final class PacketSignalBridge {
    private PacketSignalBridge() {}

    public static void onTitle(Text title) {
        IrisReporterClient.onTitleSignal(title == null ? null : title.getString());
    }

    public static void onSubtitle(Text subtitle) {
        IrisReporterClient.onSubtitleSignal(subtitle == null ? null : subtitle.getString());
    }

    public static void onTitleClear() {
        IrisReporterClient.onTitleClearSignal();
    }

    public static void onWorldEvent(String packetType, String details) {
        IrisReporterClient.onWorldSignal(packetType, details);
    }
}
