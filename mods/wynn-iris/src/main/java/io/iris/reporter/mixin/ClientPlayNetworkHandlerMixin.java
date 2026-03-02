package io.iris.reporter.mixin;

import io.iris.reporter.PacketSignalBridge;
import net.minecraft.client.network.ClientPlayNetworkHandler;
import net.minecraft.network.packet.s2c.play.ClearTitleS2CPacket;
import net.minecraft.network.packet.s2c.play.GameJoinS2CPacket;
import net.minecraft.network.packet.s2c.play.PlayerRespawnS2CPacket;
import net.minecraft.network.packet.s2c.play.SubtitleS2CPacket;
import net.minecraft.network.packet.s2c.play.TitleS2CPacket;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ClientPlayNetworkHandler.class)
public abstract class ClientPlayNetworkHandlerMixin {
    @Inject(method = "onTitle", at = @At("HEAD"))
    private void iris$onTitle(TitleS2CPacket packet, CallbackInfo ci) {
        PacketSignalBridge.onTitle(packet.text());
    }

    @Inject(method = "onSubtitle", at = @At("HEAD"))
    private void iris$onSubtitle(SubtitleS2CPacket packet, CallbackInfo ci) {
        PacketSignalBridge.onSubtitle(packet.text());
    }

    @Inject(method = "onTitleClear", at = @At("HEAD"))
    private void iris$onTitleClear(ClearTitleS2CPacket packet, CallbackInfo ci) {
        PacketSignalBridge.onTitleClear();
    }

    @Inject(method = "onGameJoin", at = @At("HEAD"))
    private void iris$onGameJoin(GameJoinS2CPacket packet, CallbackInfo ci) {
        PacketSignalBridge.onWorldEvent("GameJoinS2CPacket", packet.toString());
    }

    @Inject(method = "onPlayerRespawn", at = @At("HEAD"))
    private void iris$onPlayerRespawn(PlayerRespawnS2CPacket packet, CallbackInfo ci) {
        PacketSignalBridge.onWorldEvent("PlayerRespawnS2CPacket", packet.toString());
    }
}
