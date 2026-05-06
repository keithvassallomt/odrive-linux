// SPDX-License-Identifier: AGPL-3.0-or-later
//
// KFileItemActionPlugin — replaces the static .desktop service menu
// with a dynamic right-click integration. Equivalent of Nautilus's
// `MenuProvider`: inspects the selection at right-click time and
// returns context-appropriate actions (Sync only on placeholders,
// Unsync only on materialised, etc.). Hides entirely when nothing in
// the selection lives under an odrive mount.
#pragma once

#include <KAbstractFileItemActionPlugin>

class OdriveAction : public KAbstractFileItemActionPlugin {
    Q_OBJECT
public:
    explicit OdriveAction(QObject *parent, const QVariantList &args = {});

    QList<QAction *> actions(const KFileItemListProperties &fileItemInfos,
                             QWidget *parentWidget) override;
};
