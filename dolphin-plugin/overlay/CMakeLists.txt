kcoreaddons_add_plugin(odriveoverlay
    INSTALL_NAMESPACE "kf6/overlayicon"
    SOURCES
        OdriveOverlay.cpp
        OdriveOverlay.h
)
target_link_libraries(odriveoverlay PRIVATE
    Qt6::Core
    KF6::CoreAddons
    KF6::KIOCore
    odrive_common
)
