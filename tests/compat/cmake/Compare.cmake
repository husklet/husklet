file(SHA256 "${GENERATED}" _generated)
file(SHA256 "${EXPECTED}" _expected)
if(NOT _generated STREQUAL _expected)
  message(FATAL_ERROR
    "rebuild drift: ${GENERATED} hashes to ${_generated}, expected ${_expected}")
endif()
