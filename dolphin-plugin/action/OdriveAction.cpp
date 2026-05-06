// SPDX-License-Identifier: AGPL-3.0-or-later
#include "OdriveAction.h"
#include "OdriveContext.h"

#include <KFileItem>
#include <KFileItemListProperties>
#include <KPluginFactory>

#include <QAction>
#include <QIcon>
#include <QMenu>
#include <QProcess>
#include <QUrl>

K_PLUGIN_CLASS_WITH_JSON(OdriveAction, "odrive_action.json")

namespace {

bool isPlaceholder(const QString &path)
{
    return path.endsWith(QStringLiteral(".cloud"))
        || path.endsWith(QStringLiteral(".cloudf"));
}

// Spawn the CLI in detached mode so right-clicks return immediately —
// this mirrors the Nautilus extension's `subprocess.run(check=False)`
// fire-and-forget per file. Output goes nowhere; user-visible feedback
// comes from notify-send (in copy-share-link / open-web-preview) or
// Dolphin's directory refresh.
void launchCli(const QString &cli, const QStringList &args)
{
    if (cli.isEmpty()) {
        return;
    }
    QProcess::startDetached(cli, args);
}

} // namespace

OdriveAction::OdriveAction(QObject *parent, const QVariantList &args)
    : KAbstractFileItemActionPlugin(parent)
{
    Q_UNUSED(args);
}

QList<QAction *> OdriveAction::actions(const KFileItemListProperties &props,
                                       QWidget *parentWidget)
{
    auto &ctx = odrive::Context::instance();
    const QString cli = ctx.cliPath();

    // Filter to selection items that live under an odrive mount. Items
    // outside any mount are silently dropped — if the result is empty,
    // we return no actions and Dolphin shows no Odrive menu at all.
    QStringList paths;
    QStringList placeholders;
    QStringList materialised;
    paths.reserve(props.items().size());
    for (const KFileItem &item : props.items()) {
        const QUrl url = item.url();
        if (!url.isLocalFile()) {
            continue;
        }
        const QString p = url.toLocalFile();
        if (!ctx.isInMount(p)) {
            continue;
        }
        paths.append(p);
        if (isPlaceholder(p)) {
            placeholders.append(p);
        } else {
            materialised.append(p);
        }
    }

    if (paths.isEmpty()) {
        return {};
    }

    QMenu *submenu = new QMenu(parentWidget);

    // Sync — meaningful only for placeholders. Mirrors the Nautilus
    // extension's contextual hiding: no greyed-out entries.
    if (!placeholders.isEmpty()) {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("download")),
                                        QStringLiteral("Sync"));
        a->setToolTip(QStringLiteral("Download from cloud"));
        QObject::connect(a, &QAction::triggered, parentWidget, [cli, placeholders]() {
            for (const QString &p : placeholders) {
                launchCli(cli, {QStringLiteral("sync"), p});
            }
        });
    }

    // Unsync — only for materialised files (reverts to placeholder).
    if (!materialised.isEmpty()) {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("media-eject")),
                                        QStringLiteral("Unsync"));
        a->setToolTip(QStringLiteral("Revert to placeholder"));
        QObject::connect(a, &QAction::triggered, parentWidget, [cli, materialised]() {
            for (const QString &p : materialised) {
                launchCli(cli, {QStringLiteral("unsync"), p});
            }
        });
    }

    // Refresh — works on anything inside a mount.
    {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("view-refresh")),
                                        QStringLiteral("Refresh"));
        a->setToolTip(QStringLiteral("Re-check remote for changes"));
        QObject::connect(a, &QAction::triggered, parentWidget, [cli, paths]() {
            for (const QString &p : paths) {
                launchCli(cli, {QStringLiteral("refresh"), p});
            }
        });
    }

    // Copy Share Link — single CLI invocation with all paths so the
    // resulting URLs land on the clipboard in one shot rather than
    // racing N parallel writers.
    {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("edit-copy")),
                                        QStringLiteral("Copy Share Link"));
        a->setToolTip(QStringLiteral("Generate a share link and copy it to the clipboard"));
        QObject::connect(a, &QAction::triggered, parentWidget, [cli, paths]() {
            QStringList args = {QStringLiteral("copy-share-link")};
            args.append(paths);
            launchCli(cli, args);
        });
    }

    // Open Web Preview — only meaningful per item; on multi-select we
    // emit one xdg-open per path and let the browser handle tab
    // batching.
    {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("internet-web-browser")),
                                        QStringLiteral("Open Web Preview"));
        a->setToolTip(QStringLiteral("Open this item in the odrive web app"));
        QObject::connect(a, &QAction::triggered, parentWidget, [cli, paths]() {
            for (const QString &p : paths) {
                launchCli(cli, {QStringLiteral("open-web-preview"), p});
            }
        });
    }

    // Share Storage — static link to odrive's Spaces feature page; no
    // per-item state involved, so it ignores the selection.
    {
        QAction *a = submenu->addAction(QIcon::fromTheme(QStringLiteral("emblem-shared")),
                                        QStringLiteral("Share Storage"));
        a->setToolTip(QStringLiteral("Learn about odrive Spaces (sharing storage with others)"));
        QObject::connect(a, &QAction::triggered, parentWidget, []() {
            QProcess::startDetached(QStringLiteral("xdg-open"),
                                    {QStringLiteral("https://www.odrive.com/features/spaces")});
        });
    }

    QAction *parentAction = new QAction(QIcon::fromTheme(QStringLiteral("odrive-menu")),
                                        QStringLiteral("Odrive"), parentWidget);
    parentAction->setMenu(submenu);
    return {parentAction};
}

#include "OdriveAction.moc"
