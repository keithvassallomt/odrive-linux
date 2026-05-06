// SPDX-License-Identifier: AGPL-3.0-or-later
//
// KOverlayIconPlugin — paints odrive-syncing / odrive-synced overlays
// on file/folder icons inside Dolphin. Equivalent of Nautilus's
// `InfoProvider::update_file_info`. Reads the same SQLite tables and
// config TOML the Nautilus extension does, so emblem precedence is
// identical across both file managers.
#pragma once

#include <KOverlayIconPlugin>

class OdriveOverlay : public KOverlayIconPlugin {
    Q_OBJECT
    // KF6's KOverlayIconManager loads overlay plugins via QPluginLoader
    // with this exact IID — *not* via the KPluginFactory machinery the
    // action plugin uses, so K_PLUGIN_CLASS_WITH_JSON is wrong here.
    // Without this macro the .so loads (Qt sees the lib) but the plugin
    // is never instantiated and getOverlays is never called.
    Q_PLUGIN_METADATA(IID "org.kde.overlayicon.odriveoverlay"
                      FILE "odrive_overlay.json")
public:
    explicit OdriveOverlay(QObject *parent = nullptr);

    QStringList getOverlays(const QUrl &url) override;
};
