// SPDX-License-Identifier: AGPL-3.0-or-later
#include "OdriveOverlay.h"
#include "OdriveContext.h"

#include <QFileInfo>
#include <QUrl>

OdriveOverlay::OdriveOverlay(QObject *parent)
    : KOverlayIconPlugin(parent)
{
}

QStringList OdriveOverlay::getOverlays(const QUrl &url)
{
    if (!url.isLocalFile()) {
        return {};
    }
    const QString path = url.toLocalFile();

    auto &ctx = odrive::Context::instance();
    if (!ctx.isInMount(path)) {
        return {};
    }

    // Precedence is exactly the Nautilus extension's:
    //   1. odrive-syncing (highest) when the canonical path — i.e. with
    //      any `.cloud(f)` suffix stripped — is in sync_in_progress.
    //   2. odrive-synced for in-mount non-mount-root regular files,
    //      and for directories that have a sync rule (the rule
    //      promises the folder is kept in sync, so the emblem is
    //      honest; folders without a rule may still hold placeholders
    //      and badging the wrapper would mislead).
    //   3. nothing.
    // Each emblem is gated by the user-toggleable preference. Even
    // with the toggle off, the syncing-state branch returns early
    // (without an emblem) so a stale synced emblem doesn't leak
    // through during a sync.
    const QString stripped = odrive::Context::stripPlaceholderSuffix(path);
    if (ctx.isSyncInProgress(stripped)) {
        if (ctx.syncingEmblemEnabled()) {
            return {QStringLiteral("odrive-syncing")};
        }
        return {};
    }

    if (!ctx.syncedEmblemEnabled()) {
        return {};
    }

    // Placeholders never get an emblem — the cloud-file-type icon
    // already conveys "remote".
    if (path.endsWith(QStringLiteral(".cloud"))
        || path.endsWith(QStringLiteral(".cloudf"))) {
        return {};
    }

    if (ctx.isMountRoot(path)) {
        return {};
    }

    QFileInfo info(path);
    if (info.isDir()) {
        if (ctx.hasFolderRule(path)) {
            return {QStringLiteral("odrive-synced")};
        }
        return {};
    }

    return {QStringLiteral("odrive-synced")};
}
