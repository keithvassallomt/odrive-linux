add_library(odrive_common STATIC
    OdriveContext.cpp
    OdriveContext.h
)
target_include_directories(odrive_common PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(odrive_common
    PUBLIC
        Qt6::Core
    PRIVATE
        SQLite::SQLite3
)
# PIC required because both plugin .so files link this static lib in.
set_target_properties(odrive_common PROPERTIES POSITION_INDEPENDENT_CODE ON)
