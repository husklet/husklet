cmake_minimum_required(VERSION 3.20)
project(husklet_native_quality LANGUAGES C)
option(HL_BUILD_TESTS "Build linter contracts" ON)
set(HL_LINT ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
if(HL_BUILD_TESTS)
  enable_testing()
endif()
file(GLOB_RECURSE HL_EXEC_C CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/exec/src/*.c" "${CMAKE_CURRENT_SOURCE_DIR}/exec/cache/*.c")
set(HL_RUNTIME_C
    "${CMAKE_CURRENT_SOURCE_DIR}/address_projection.c"
    "${CMAKE_CURRENT_SOURCE_DIR}/executable_authority.c"
    "${CMAKE_CURRENT_SOURCE_DIR}/shim.c")
add_library(hl_lint_native_objects OBJECT EXCLUDE_FROM_ALL ${HL_EXEC_C} ${HL_RUNTIME_C})
target_compile_features(hl_lint_native_objects PRIVATE c_std_11)
set_source_files_properties(${HL_RUNTIME_C} PROPERTIES COMPILE_DEFINITIONS _GNU_SOURCE)
target_include_directories(hl_lint_native_objects PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}" "${CMAKE_CURRENT_SOURCE_DIR}/retained/include" "${CMAKE_CURRENT_SOURCE_DIR}/retained/src" "${CMAKE_CURRENT_SOURCE_DIR}/exec/include" "${CMAKE_CURRENT_SOURCE_DIR}/exec/src" "${CMAKE_CURRENT_SOURCE_DIR}/cpu/include")

# Reconstruct each production retained-C compile group from the same manifest
# Cargo consumes.  Besides keeping clang-tidy's compile database honest, these
# object targets make strict warnings a real gate instead of an unused CMake
# target that merely happens to configure successfully.
file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/retained/COMPILED_TUS.tsv" HL_RETAINED_ROWS)
list(POP_FRONT HL_RETAINED_ROWS)
set(HL_RETAINED_GROUPS normal_archive target_aarch64_direct target_x86_64_direct lifecycle_aarch64_direct lifecycle_x86_64_direct)
foreach(row IN LISTS HL_RETAINED_ROWS)
  string(REPLACE "\t" ";" columns "${row}")
  list(GET columns 0 group)
  if(group IN_LIST HL_RETAINED_GROUPS)
    list(GET columns 2 source)
    list(APPEND HL_RETAINED_${group}_SOURCES "${CMAKE_CURRENT_SOURCE_DIR}/retained/${source}")
  endif()
endforeach()
set(HL_RETAINED_TUS)
set(HL_STRICT_WARNING_TARGETS hl_lint_native_objects)
foreach(group IN LISTS HL_RETAINED_GROUPS)
  if(NOT HL_RETAINED_${group}_SOURCES)
    message(FATAL_ERROR "retained compile group ${group} is empty")
  endif()
  set(target "hl_lint_retained_${group}")
  add_library(${target} OBJECT EXCLUDE_FROM_ALL ${HL_RETAINED_${group}_SOURCES})
  target_compile_features(${target} PRIVATE c_std_11)
  target_compile_definitions(${target} PRIVATE _GNU_SOURCE HL_ENABLE_LOGGING=0 HL_TRANSLIT_DEFAULT=0)
  if(group STREQUAL "target_aarch64_direct")
    target_compile_definitions(${target} PRIVATE HL_EMBEDDED_BUILD=1 HL_ENGINE_NO_MAIN=1 HL_ENGINE_NO_STANDALONE=1 HL_TARGET_NAMESPACE=aarch64)
  elseif(group STREQUAL "target_x86_64_direct")
    target_compile_definitions(${target} PRIVATE HL_EMBEDDED_BUILD=1 HL_ENGINE_NO_MAIN=1 HL_ENGINE_NO_STANDALONE=1 HL_TARGET_NAMESPACE=x86_64)
  elseif(group STREQUAL "lifecycle_aarch64_direct")
    target_compile_definitions(${target} PRIVATE HL_EMBEDDED_BUILD=1 HL_TARGET_NAMESPACE=aarch64 HL_PRODUCTION_GUEST_ISA=HL_GUEST_ISA_AARCH64)
  elseif(group STREQUAL "lifecycle_x86_64_direct")
    target_compile_definitions(${target} PRIVATE HL_EMBEDDED_BUILD=1 HL_TARGET_NAMESPACE=x86_64 HL_PRODUCTION_GUEST_ISA=HL_GUEST_ISA_X86_64)
  else()
    target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Wstrict-prototypes -Wmissing-prototypes -Werror=implicit-function-declaration -Werror=implicit-int)
  endif()
  target_include_directories(${target} PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}" "${CMAKE_CURRENT_SOURCE_DIR}/retained/include" "${CMAKE_CURRENT_SOURCE_DIR}/retained/src")
  list(APPEND HL_RETAINED_TUS ${HL_RETAINED_${group}_SOURCES})
  list(APPEND HL_STRICT_WARNING_TARGETS ${target})
endforeach()
list(REMOVE_DUPLICATES HL_RETAINED_TUS)
add_custom_target(strict-warnings-c DEPENDS ${HL_STRICT_WARNING_TARGETS})

add_subdirectory(linter)
add_custom_target(source-manifest-check COMMAND "${CMAKE_COMMAND}" -DROOT=${CMAKE_CURRENT_SOURCE_DIR}/../.. -DMANIFEST=${CMAKE_CURRENT_SOURCE_DIR}/lint-sources.manifest -P "${CMAKE_CURRENT_SOURCE_DIR}/cmake/SourceManifest.cmake" VERBATIM)
add_dependencies(hl_lint source-manifest-check)
find_program(HL_CLANG_FORMAT clang-format REQUIRED)
find_program(HL_CLANG_TIDY clang-tidy REQUIRED)
find_program(HL_CPPCHECK cppcheck REQUIRED)
set(HL_CLANG_TIDY_CHECKS "clang-analyzer-*,-clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling,-clang-analyzer-security.MmapWriteExec,-clang-analyzer-unix.BlockInCriticalSection,bugprone-assignment-in-if-condition,bugprone-branch-clone,bugprone-inc-dec-in-conditions,bugprone-infinite-loop,bugprone-not-null-terminated-result,bugprone-posix-return,bugprone-signal-handler,bugprone-sizeof-expression,bugprone-suspicious-memory-comparison,bugprone-suspicious-memset-usage,bugprone-undefined-memory-manipulation")
file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/retained/RUNTIME_SOURCES.manifest" HL_RETAINED_SOURCE_ROWS)
set(HL_RETAINED_SOURCES)
foreach(source IN LISTS HL_RETAINED_SOURCE_ROWS)
  if(source MATCHES "[.](c|h)$")
    list(APPEND HL_RETAINED_SOURCES "${CMAKE_CURRENT_SOURCE_DIR}/retained/${source}")
  endif()
endforeach()
set(HL_LINT_SOURCES ${HL_EXEC_C} ${HL_RUNTIME_C} ${HL_RETAINED_SOURCES})
set(HL_TIDY_ARGS)
set(HL_SOURCE_ARGS)
set(HL_ALLOW_STDIO
    src/core/target/aarch64.c
    src/core/target/x86_64.c
    src/linux_abi/checkpoint.c
    src/linux_abi/container/netns.c
    src/linux_abi/container/state.c
    src/linux_abi/elf.c
    src/linux_abi/fork.c
    src/linux_abi/parse.c
    src/linux_abi/sentry.c
    src/linux_abi/syscall/dispatch.c
    src/linux_abi/syscall/event.c
    src/linux_abi/syscall/inotify.c
    src/linux_abi/syscall/io.c
    src/linux_abi/syscall/proc.c
    src/linux_abi/x86.c
    src/translator/guest/aarch64/cache.c
    src/translator/guest/aarch64/signal.c
    src/translator/guest/aarch64/translate.c
    src/translator/guest/x86_64/avx.c
    src/translator/guest/x86_64/cache.c
    src/translator/guest/x86_64/dispatch.h
    src/translator/guest/x86_64/signal.c
    src/translator/guest/x86_64/translate.c
    tools/lifecycle_e2e_runner.c)
foreach(source IN LISTS HL_RETAINED_normal_archive_SOURCES)
  list(APPEND HL_TIDY_ARGS --clang-tidy-source-file "${source}")
endforeach()
foreach(source IN LISTS HL_LINT_SOURCES)
  list(APPEND HL_SOURCE_ARGS --source-file "${source}")
endforeach()
foreach(source IN LISTS HL_ALLOW_STDIO)
  list(APPEND HL_SOURCE_ARGS --allow-stdio-file "runtime/native/retained/${source}")
endforeach()
add_custom_target(lint-c COMMAND $<TARGET_FILE:hl_lint> --strict --compile-commands-dir "${CMAKE_BINARY_DIR}" --clang-format-bin "${HL_CLANG_FORMAT}" --clang-tidy-bin "${HL_CLANG_TIDY}" --cppcheck-bin "${HL_CPPCHECK}" --clang-tidy-checks "${HL_CLANG_TIDY_CHECKS}" --include-dir "${CMAKE_CURRENT_SOURCE_DIR}" --include-dir "${CMAKE_CURRENT_SOURCE_DIR}/retained/include" --include-dir "${CMAKE_CURRENT_SOURCE_DIR}/retained/src" --include-dir "${CMAKE_CURRENT_SOURCE_DIR}/exec/include" --include-dir "${CMAKE_CURRENT_SOURCE_DIR}/exec/src" --clang-format-check --clang-tidy-check --cppcheck-check ${HL_SOURCE_ARGS} ${HL_TIDY_ARGS} DEPENDS hl_lint source-manifest-check strict-warnings-c WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/../.." USES_TERMINAL COMMAND_EXPAND_LISTS)
add_custom_target(fmt-c COMMAND "${HL_CLANG_FORMAT}" -i ${HL_LINT_SOURCES} USES_TERMINAL COMMAND_EXPAND_LISTS)
add_custom_target(fmt-c-check COMMAND "${HL_CLANG_FORMAT}" --dry-run --Werror ${HL_LINT_SOURCES} USES_TERMINAL COMMAND_EXPAND_LISTS)
