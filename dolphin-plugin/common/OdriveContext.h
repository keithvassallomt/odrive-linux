// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Shared helpers for the odrive-linux Dolphin plugins. Mirrors the
// caching / discovery logic in nautilus_extension.py but in C++/Qt:
//
//   - Mount discovery via QProcess shelling `odrive-cli mounts --paths`,
//     cached for the process lifetime (Dolphin restart picks up new
//     mounts; same contract as the Nautilus extension).
//   - Live state from ~/.odrive-linux.db (sync_in_progress and
//     folder_sync_rules tables) read directly via the C sqlite3 API,
//     cached with a short TTL so the overlay plugin can poll cheaply.
//   - Emblem-toggle preferences from ~/.config/odrive-linux/config.toml,
//     also TTL-cached.
//   - odrive-cli location resolution: $ODRIVE_CLI override, then PATH,
//     then workspace target/ fallbacks.
//
// All methods are safe to call from any thread; an internal QMutex
// serialises cache refresh.
#pragma once

#include <QDateTime>
#include <QMutex>
#include <QSet>
#include <QString>
#include <QStringList>

namespace odrive {

class Context {
public:
    static Context &instance();

    // Active mount roots (absolute local paths). Populated lazily on
    // first call, cached forever — restart Dolphin to pick up newly
    // added mounts (same contract Nautilus has).
    QStringList mounts();

    // True if `path` is at or below any mount root. The check is
    // textual (string prefix + path-separator boundary) — we don't
    // canonicalise symlinks, matching the Nautilus extension's
    // behaviour so users get consistent results across both file
    // managers.
    bool isInMount(const QString &path);

    // True iff `path` equals one of the mount roots. The mount root
    // itself never gets the synced emblem (mirrors Nautilus); also
    // useful to suppress per-item actions that don't make sense at
    // the root (Unsync of a mount would be a re-mount).
    bool isMountRoot(const QString &path);

    // Drop a trailing `.cloud`/`.cloudf` so callers can match the
    // canonical path that lives in the sync_in_progress / folder rule
    // tables. The GUI marks the conceptual folder path; on disk the
    // entry is still `<path>.cloudf` until expansion completes.
    static QString stripPlaceholderSuffix(const QString &path);

    // True if `<path>` (already stripped of any `.cloud(f)` suffix by
    // the caller) appears in the GUI's sync_in_progress table. The
    // backing read is TTL-cached at 0.5s so the overlay plugin can
    // poll without hammering SQLite.
    bool isSyncInProgress(const QString &path);

    // True if `path` has a row in the folder_sync_rules table — the
    // source of truth for "user set a sync rule via the Manager."
    // Same 0.5s cache as sync_in_progress (single SQLite open per
    // refresh covers both tables).
    bool hasFolderRule(const QString &path);

    // User-toggleable emblem prefs persisted by the GUI's Preferences
    // → Appearance page. Both default true. 1s TTL cache so flipping
    // the switch takes effect within a directory tour.
    bool syncedEmblemEnabled();
    bool syncingEmblemEnabled();

    // Absolute path to the odrive-cli binary, resolved once and cached
    // for the process lifetime. Empty string if no usable binary was
    // found — callers should treat that as "plugin disabled" rather
    // than spawning bogus QProcesses.
    QString cliPath();

private:
    Context();
    Context(const Context &) = delete;
    Context &operator=(const Context &) = delete;

    void ensureMounts();
    void refreshDb();
    void refreshConfig();

    QMutex m_mountsMutex;
    bool m_mountsLoaded = false;
    QStringList m_mounts;
    QString m_cliPath;
    bool m_cliPathLoaded = false;

    QMutex m_dbMutex;
    QSet<QString> m_syncInProgress;
    QSet<QString> m_folderRules;
    QDateTime m_dbExpires;

    QMutex m_configMutex;
    bool m_syncedEmblem = true;
    bool m_syncingEmblem = true;
    QDateTime m_configExpires;
};

} // namespace odrive
