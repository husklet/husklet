if(NOT DEFINED HL_LINT OR NOT DEFINED HL_LINT_SOURCE_DIR OR NOT DEFINED HL_LINT_CASE)
  message(FATAL_ERROR "HL_LINT, HL_LINT_SOURCE_DIR and HL_LINT_CASE are required")
endif()

set(_common
  --skip-clang-format
  --skip-clang-tidy
  --skip-cppcheck)

if(HL_LINT_CASE STREQUAL "clean")
  set(_expected 0)
  set(_pattern "warnings=0 errors=0")
  set(_args
    ${_common}
    --strict
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/fixture.c")
elseif(HL_LINT_CASE STREQUAL "warning-nonstrict")
  set(_expected 0)
  set(_pattern "warnings=[1-9][0-9]* \\(non-fatal\\)")
  set(_args
    ${_common}
    --max-line-length 8
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/fixture.c")
elseif(HL_LINT_CASE STREQUAL "warning-strict")
  set(_expected 1)
  set(_pattern "strict mode enabled")
  set(_args
    ${_common}
    --strict
    --max-line-length 8
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/fixture.c")
elseif(HL_LINT_CASE STREQUAL "error")
  set(_expected 1)
  set(_pattern "direct environment access is only allowed in explicitly whitelisted files")
  set(_args
    ${_common}
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/getenv_fixture.c")
elseif(HL_LINT_CASE STREQUAL "environment-extended-error")
  set(_expected 1)
  set(_pattern "warnings=0 errors=5")
  set(_args
    ${_common}
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/environment_extended_fixture.c")
elseif(HL_LINT_CASE STREQUAL "stdio-error")
  set(_expected 1)
  set(_pattern "direct console output is forbidden; use tagged logging")
  set(_args
    ${_common}
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/stdio_fixture.c")
elseif(HL_LINT_CASE STREQUAL "stdio-allowed")
  set(_expected 0)
  set(_pattern "warnings=0 errors=0")
  set(_args
    ${_common}
    --strict
    --allow-stdio-file "linter/tests/stdio_fixture.c"
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/stdio_fixture.c")
elseif(HL_LINT_CASE STREQUAL "stdio-extended-error")
  set(_expected 1)
  set(_pattern "warnings=0 errors=4")
  set(_args
    ${_common}
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/stdio_extended_fixture.c")
elseif(HL_LINT_CASE STREQUAL "platform-debug-error")
  set(_expected 1)
  set(_pattern "warnings=0 errors=5")
  set(_args
    ${_common}
    --source-dir "${HL_LINT_SOURCE_DIR}/linter/tests/platform_debug")
elseif(HL_LINT_CASE STREQUAL "shell-error")
  set(_expected 1)
  set(_pattern "shell execution is forbidden")
  set(_args
    ${_common}
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/shell_fixture.c")
elseif(HL_LINT_CASE STREQUAL "shell-allowed")
  set(_expected 0)
  set(_pattern "warnings=0 errors=0")
  set(_args
    ${_common}
    --strict
    --allow-shell-file "linter/tests/shell_fixture.c"
    --source-file "${HL_LINT_SOURCE_DIR}/linter/tests/shell_fixture.c")
elseif(HL_LINT_CASE STREQUAL "usage")
  set(_expected 2)
  set(_pattern "unknown option")
  set(_args --not-a-real-option)
elseif(HL_LINT_CASE STREQUAL "missing-value")
  set(_expected 2)
  set(_pattern "--source-file expects a value")
  set(_args --source-file)
elseif(HL_LINT_CASE STREQUAL "invalid-integer")
  set(_expected 2)
  set(_pattern "invalid integer `twelve` for --max-line-length")
  set(_args --max-line-length twelve)
else()
  message(FATAL_ERROR "unknown lint exit test case: ${HL_LINT_CASE}")
endif()

execute_process(
  COMMAND "${HL_LINT}" ${_args}
  RESULT_VARIABLE _status
  OUTPUT_VARIABLE _output
  ERROR_VARIABLE _error)

if(NOT _status EQUAL _expected)
  message(FATAL_ERROR
    "${HL_LINT_CASE}: expected exit ${_expected}, got ${_status}\n${_output}${_error}")
endif()

if(NOT "${_output}${_error}" MATCHES "${_pattern}")
  message(FATAL_ERROR
    "${HL_LINT_CASE}: output did not match `${_pattern}`\n${_output}${_error}")
endif()
