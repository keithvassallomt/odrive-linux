cmake_minimum_required(VERSION 3.16)
project(odrive-dolphin-plugins
    VERSION 0.1.0
    DESCRIPTION "KFileItemAction + KOverlayIcon plugins giving Dolphin first-class odrive integration"
    LANGUAGES CXX
)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_INCLUDE_CURRENT_DIR ON)

set(QT_MIN_VERSION 6.5.0)
set(KF_MIN_VERSION 6.0.0)

# ECM (Extra CMake Modules) gives us KDEInstallDirs (so KDE_INSTALL_PLUGINDIR
# resolves to <prefix>/lib/x86_64-linux-gnu/qt6/plugins/ on Debian-family
# distros) and KDECMakeSettings (sane defaults — RPATH, build types, etc).
find_package(ECM ${KF_MIN_VERSION} REQUIRED NO_MODULE)
list(APPEND CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)

find_package(Qt6 ${QT_MIN_VERSION} REQUIRED COMPONENTS Core Gui Widgets)
find_package(KF6 ${KF_MIN_VERSION} REQUIRED COMPONENTS CoreAddons KIO I18n)

# We talk to ~/.odrive-linux.db directly via the C sqlite3 API rather than
# QtSql to dodge QSqlDatabase's per-thread connection requirement — the
# overlay plugin can be called from non-GUI threads and a single shared
# connection would be a footgun.
find_package(SQLite3 REQUIRED)

add_subdirectory(common)
add_subdirectory(action)
add_subdirectory(overlay)
