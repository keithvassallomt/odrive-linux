kcoreaddons_add_plugin(odriveaction
    INSTALL_NAMESPACE "kf6/kfileitemaction"
    SOURCES
        OdriveAction.cpp
        OdriveAction.h
)
target_link_libraries(odriveaction PRIVATE
    Qt6::Core
    Qt6::Gui
    Qt6::Widgets
    KF6::CoreAddons
    KF6::KIOCore
    KF6::KIOWidgets
    odrive_common
)
