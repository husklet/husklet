if(NOT HL_LINT)
  return()
endif()

if(WIN32)
  set(_hl_lint_process_source src/process_windows.c)
else()
  set(_hl_lint_process_source src/process_posix.c)
endif()

add_executable(hl_lint
  src/analyzers.c
  src/cli.c
  src/hl_lint.c
  src/policy.c
  src/sources.c
  ${_hl_lint_process_source})

target_compile_features(hl_lint PRIVATE c_std_11)
if(CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
  target_compile_options(hl_lint PRIVATE
    -O2 -g
    -Wall -Wextra -Wpedantic -Wconversion -Wshadow
    -Wstrict-prototypes -Wmissing-prototypes
    -Werror=implicit-function-declaration -Werror=implicit-int)
  if(NOT WIN32)
    target_compile_options(hl_lint PRIVATE -fvisibility=hidden)
  endif()
endif()

set_target_properties(hl_lint PROPERTIES
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tools")

if(HL_BUILD_TESTS)
  add_executable(hl_lint_fake_analyzer
    tests/fake_analyzer.c)
  set_target_properties(hl_lint_fake_analyzer PROPERTIES
    OUTPUT_NAME "hl lint fake analyzer"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tools")

  add_executable(hl_lint_process_helper
    tests/process_helper.c)
  set_target_properties(hl_lint_process_helper PROPERTIES
    OUTPUT_NAME "hl lint process helper"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tools")

  add_executable(hl_lint_process_test
    tests/test_process.c
    ${_hl_lint_process_source})
  target_include_directories(hl_lint_process_test PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/src")
  set_target_properties(hl_lint_process_test PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tools")

  add_test(NAME lint.process
    COMMAND $<TARGET_FILE:hl_lint_process_test>
      $<TARGET_FILE:hl_lint_process_helper>)
  set_tests_properties(lint.process PROPERTIES LABELS "lint")

  add_test(NAME lint.clang-tidy-argv
    COMMAND $<TARGET_FILE:hl_lint>
      --strict
      --skip-clang-format
      --skip-cppcheck
      --skip-custom
      --clang-tidy-bin $<TARGET_FILE:hl_lint_fake_analyzer>
      --compile-commands-dir "${CMAKE_BINARY_DIR}"
      --clang-tidy-checks "bugprone-*,performance-*"
      --clang-tidy-source-file "${CMAKE_CURRENT_SOURCE_DIR}/tests/fixture.c"
      --source-file "${CMAKE_CURRENT_SOURCE_DIR}/tests/fixture.c")
  set_tests_properties(lint.clang-tidy-argv PROPERTIES
    LABELS "lint"
    PASS_REGULAR_EXPRESSION "fake-analyzer: clang-tidy argv ok")

  add_test(NAME lint.cppcheck-argv
    COMMAND $<TARGET_FILE:hl_lint>
      --strict
      --skip-clang-format
      --skip-clang-tidy
      --skip-custom
      --cppcheck-bin $<TARGET_FILE:hl_lint_fake_analyzer>
      --compile-commands-dir "${CMAKE_BINARY_DIR}"
      --source-file "${CMAKE_CURRENT_SOURCE_DIR}/tests/fixture.c")
  set_tests_properties(lint.cppcheck-argv PROPERTIES
    LABELS "lint"
    PASS_REGULAR_EXPRESSION "fake-analyzer: cppcheck argv ok")

  foreach(_case IN ITEMS clean warning-nonstrict warning-strict error environment-extended-error
      stdio-error stdio-allowed stdio-extended-error platform-debug-error
      shell-error shell-allowed usage missing-value invalid-integer)
    add_test(NAME lint.exit-${_case}
      COMMAND "${CMAKE_COMMAND}"
        -DHL_LINT=$<TARGET_FILE:hl_lint>
        -DHL_LINT_SOURCE_DIR=${CMAKE_SOURCE_DIR}
        -DHL_LINT_CASE=${_case}
        -P "${CMAKE_CURRENT_SOURCE_DIR}/tests/assert_exit.cmake")
    set_tests_properties(lint.exit-${_case} PROPERTIES LABELS "lint")
  endforeach()
endif()
