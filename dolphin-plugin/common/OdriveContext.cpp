// SPDX-License-Identifier: AGPL-3.0-or-later
#include "OdriveContext.h"

#include <QByteArray>
#include <QCoreApplication>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QMutexLocker>
#include <QProcess>
#include <QStandardPaths>
#include <QTextStream>

#include <sqlite3.h>

namespace odrive {

namespace {

constexpr int kDbCacheMs = 500;
constexpr int kConfigCacheMs = 1000;

QString homePath()
{
    return QDir::homePath();
}

QString dbPath()
{
    return homePath() + QStringLiteral("/.odrive-linux.db");
}

QString configPath()
{
    return homePath() + QStringLiteral("/.config/odrive-linux/config.toml");
}

// Match the Nautilus extension's locator: $ODRIVE_CLI override → PATH →
// workspace fallbacks (release / debug). The plugin .so doesn't have a
// stable on-disk relationship to the workspace, so the workspace
// fallbacks try the developer's typical layout (CWD = the repo root)
// and the user's home as a last resort.
QString locateCli()
{
    const QByteArray override = qgetenv("ODRIVE_CLI");
    if (!override.isEmpty()) {
        const QString path = QString::fromLocal8Bit(override);
        if (QFileInfo(path).isExecutable()) {
            return path;
        }
    }
    const QString onPath = QStandardPaths::findExecutable(QStringLiteral("odrive-cli"));
    if (!onPath.isEmpty()) {
        return onPath;
    }
    const QStringList candidates = {
        QDir::currentPath() + QStringLiteral("/target/release/odrive-cli"),
        QDir::currentPath() + QStringLiteral("/target/debug/odrive-cli"),
    };
    for (const QString &c : candidates) {
        if (QFileInfo(c).isExecutable()) {
            return c;
        }
    }
    return QString();
}

} // namespace

Context &Context::instance()
{
    static Context ctx;
    return ctx;
}

Context::Context() = default;

void Context::ensureMounts()
{
    if (m_mountsLoaded) {
        return;
    }
    m_mountsLoaded = true;

    // Resolve the CLI path inline rather than calling cliPath() — the
    // caller already holds m_mountsMutex, and cliPath() would try to
    // re-lock it, deadlocking on Qt's non-recursive QMutex (which in
    // some builds asserts and crashes Dolphin instead of hanging).
    if (!m_cliPathLoaded) {
        m_cliPathLoaded = true;
        m_cliPath = locateCli();
    }
    if (m_cliPath.isEmpty()) {
        return;
    }

    QProcess p;
    p.start(m_cliPath, {QStringLiteral("mounts"), QStringLiteral("--paths")});
    if (!p.waitForStarted(2000)) {
        return;
    }
    if (!p.waitForFinished(5000)) {
        p.kill();
        return;
    }
    if (p.exitStatus() != QProcess::NormalExit || p.exitCode() != 0) {
        return;
    }
    const QString out = QString::fromUtf8(p.readAllStandardOutput());
    for (const QString &line : out.split(QLatin1Char('\n'))) {
        const QString trimmed = line.trimmed();
        if (!trimmed.isEmpty()) {
            m_mounts.append(trimmed);
        }
    }
}

QStringList Context::mounts()
{
    QMutexLocker lock(&m_mountsMutex);
    ensureMounts();
    return m_mounts;
}

bool Context::isInMount(const QString &path)
{
    if (path.isEmpty()) {
        return false;
    }
    QMutexLocker lock(&m_mountsMutex);
    ensureMounts();
    for (const QString &mount : std::as_const(m_mounts)) {
        if (path == mount) {
            return true;
        }
        // Boundary-check the trailing separator so /home/keith/odrive2 is
        // not treated as inside /home/keith/odrive.
        if (path.startsWith(mount) && path.length() > mount.length()
            && path.at(mount.length()) == QLatin1Char('/')) {
            return true;
        }
    }
    return false;
}

bool Context::isMountRoot(const QString &path)
{
    QMutexLocker lock(&m_mountsMutex);
    ensureMounts();
    return m_mounts.contains(path);
}

QString Context::stripPlaceholderSuffix(const QString &path)
{
    if (path.endsWith(QStringLiteral(".cloudf"))) {
        return path.left(path.length() - 7);
    }
    if (path.endsWith(QStringLiteral(".cloud"))) {
        return path.left(path.length() - 6);
    }
    return path;
}

void Context::refreshDb()
{
    const QDateTime now = QDateTime::currentDateTimeUtc();
    if (m_dbExpires.isValid() && now < m_dbExpires) {
        return;
    }

    QSet<QString> sip;
    QSet<QString> rules;

    sqlite3 *db = nullptr;
    const QString uri = QStringLiteral("file:%1?mode=ro").arg(dbPath());
    const QByteArray uriUtf8 = uri.toUtf8();
    const int rc = sqlite3_open_v2(uriUtf8.constData(), &db,
                                   SQLITE_OPEN_READONLY | SQLITE_OPEN_URI, nullptr);
    if (rc != SQLITE_OK) {
        if (db) {
            sqlite3_close(db);
        }
        // Missing DB / unparseable / locked: fall back to empty sets and
        // re-cache so we don't hammer the open every call.
        m_syncInProgress = sip;
        m_folderRules = rules;
        m_dbExpires = now.addMSecs(kDbCacheMs);
        return;
    }

    // Sub-second busy timeout — if another process is writing we don't
    // want to block the Dolphin UI thread, just return what we can.
    sqlite3_busy_timeout(db, 200);

    sqlite3_stmt *stmt = nullptr;
    if (sqlite3_prepare_v2(db, "SELECT local_path FROM sync_in_progress", -1, &stmt, nullptr) == SQLITE_OK) {
        while (sqlite3_step(stmt) == SQLITE_ROW) {
            const unsigned char *text = sqlite3_column_text(stmt, 0);
            if (text) {
                sip.insert(QString::fromUtf8(reinterpret_cast<const char *>(text)));
            }
        }
        sqlite3_finalize(stmt);
        stmt = nullptr;
    }

    if (sqlite3_prepare_v2(db, "SELECT local_path FROM folder_sync_rules", -1, &stmt, nullptr) == SQLITE_OK) {
        while (sqlite3_step(stmt) == SQLITE_ROW) {
            const unsigned char *text = sqlite3_column_text(stmt, 0);
            if (text) {
                rules.insert(QString::fromUtf8(reinterpret_cast<const char *>(text)));
            }
        }
        sqlite3_finalize(stmt);
    }

    sqlite3_close(db);

    m_syncInProgress = sip;
    m_folderRules = rules;
    m_dbExpires = now.addMSecs(kDbCacheMs);
}

bool Context::isSyncInProgress(const QString &path)
{
    QMutexLocker lock(&m_dbMutex);
    refreshDb();
    return m_syncInProgress.contains(path);
}

bool Context::hasFolderRule(const QString &path)
{
    QMutexLocker lock(&m_dbMutex);
    refreshDb();
    return m_folderRules.contains(path);
}

void Context::refreshConfig()
{
    const QDateTime now = QDateTime::currentDateTimeUtc();
    if (m_configExpires.isValid() && now < m_configExpires) {
        return;
    }
    bool synced = true;
    bool syncing = true;

    QFile f(configPath());
    if (f.open(QIODevice::ReadOnly | QIODevice::Text)) {
        QTextStream in(&f);
        while (!in.atEnd()) {
            const QString line = in.readLine().trimmed();
            // Single-line key = bool parser, intentionally tiny — same
            // approach as the Nautilus extension. Any value that doesn't
            // start with `false` is treated as true so a corrupted
            // value preserves the on-by-default behaviour.
            auto parseBool = [](const QString &line) {
                const int eq = line.indexOf(QLatin1Char('='));
                if (eq < 0) {
                    return true;
                }
                const QString value = line.mid(eq + 1).trimmed().toLower();
                return !value.startsWith(QStringLiteral("false"));
            };
            if (line.startsWith(QStringLiteral("nautilus_synced_emblem"))) {
                synced = parseBool(line);
            } else if (line.startsWith(QStringLiteral("nautilus_syncing_emblem"))) {
                syncing = parseBool(line);
            }
        }
    }
    m_syncedEmblem = synced;
    m_syncingEmblem = syncing;
    m_configExpires = now.addMSecs(kConfigCacheMs);
}

bool Context::syncedEmblemEnabled()
{
    QMutexLocker lock(&m_configMutex);
    refreshConfig();
    return m_syncedEmblem;
}

bool Context::syncingEmblemEnabled()
{
    QMutexLocker lock(&m_configMutex);
    refreshConfig();
    return m_syncingEmblem;
}

QString Context::cliPath()
{
    QMutexLocker lock(&m_mountsMutex);
    if (!m_cliPathLoaded) {
        m_cliPathLoaded = true;
        m_cliPath = locateCli();
    }
    return m_cliPath;
}

} // namespace odrive
