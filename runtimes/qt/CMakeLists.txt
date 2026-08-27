cmake_minimum_required(VERSION 3.21)

project(orna_runtime_qt VERSION 1.0.0 LANGUAGES CXX)

find_package(Qt6 6.2 REQUIRED COMPONENTS Core Widgets)

# The ABI is a single canonical source owned by the repository spec.  Keep the
# build tree outside the source tree and expose only that directory to targets.
set(ORNA_RUNTIME_ABI_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../spec/spec")
if(NOT EXISTS "${ORNA_RUNTIME_ABI_INCLUDE_DIR}/orna_runtime_abi_v1.h")
    message(FATAL_ERROR "canonical Orna runtime ABI header was not found")
endif()

add_library(orna-runtime-qt SHARED
    src/runtime.cpp
)

target_compile_features(orna-runtime-qt PRIVATE cxx_std_17)
target_include_directories(orna-runtime-qt PRIVATE "${ORNA_RUNTIME_ABI_INCLUDE_DIR}")
target_link_libraries(orna-runtime-qt PRIVATE Qt6::Core Qt6::Widgets)

if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
    target_compile_options(orna-runtime-qt PRIVATE -Wall -Wextra -Wpedantic)
endif()
set_target_properties(orna-runtime-qt PROPERTIES
    OUTPUT_NAME "orna-runtime-qt"
    CXX_VISIBILITY_PRESET hidden
    VISIBILITY_INLINES_HIDDEN YES
)

install(
    TARGETS orna-runtime-qt
    LIBRARY DESTINATION lib/orna
)

add_executable(orna-runtime-qt-demo
    examples/runtime_demo.cpp
)
target_compile_features(orna-runtime-qt-demo PRIVATE cxx_std_17)
target_include_directories(orna-runtime-qt-demo PRIVATE "${ORNA_RUNTIME_ABI_INCLUDE_DIR}")
target_link_libraries(orna-runtime-qt-demo PRIVATE
    orna-runtime-qt
    Qt6::Core
    Qt6::Widgets
)

if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
    target_compile_options(orna-runtime-qt-demo PRIVATE -Wall -Wextra -Wpedantic)
endif()

include(CTest)
if(BUILD_TESTING)
    add_executable(orna-runtime-qt-test
        tests/runtime_test.cpp
    )
    target_compile_features(orna-runtime-qt-test PRIVATE cxx_std_17)
    target_include_directories(orna-runtime-qt-test PRIVATE "${ORNA_RUNTIME_ABI_INCLUDE_DIR}")
    target_link_libraries(orna-runtime-qt-test PRIVATE
        orna-runtime-qt
        Qt6::Core
        Qt6::Widgets
    )

    if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
        target_compile_options(orna-runtime-qt-test PRIVATE -Wall -Wextra -Wpedantic)
    endif()
    add_test(NAME orna-runtime-qt-contract COMMAND orna-runtime-qt-test)
    set_tests_properties(orna-runtime-qt-contract PROPERTIES
        ENVIRONMENT "QT_QPA_PLATFORM=offscreen"
    )
endif()
