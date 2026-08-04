set(_HL_CORPUS_CMAKE_DIR "${CMAKE_CURRENT_LIST_DIR}")

function(hl_compat_smoke target)
  set(_root "${_HL_CORPUS_CMAKE_DIR}/..")
  cmake_path(NORMAL_PATH _root)
  set(_hashes "${_root}/prebuilt/manifest.tsv")
  execute_process(
    COMMAND "${CMAKE_COMMAND}" -DROOT=${_root} -DMANIFEST=${_hashes}
            -P "${_HL_CORPUS_CMAKE_DIR}/Verify.cmake"
    RESULT_VARIABLE _verified)
  if(NOT _verified EQUAL 0)
    message(FATAL_ERROR "checked compatibility prebuilts failed drift verification")
  endif()

  if(HL_CORPUS_MODE STREQUAL "PREBUILT")
    file(STRINGS "${_hashes}" _rows)
    set(_artifacts "")
    foreach(_row IN LISTS _rows)
      if(_row MATCHES "^#")
        continue()
      endif()
      string(REPLACE "\t" ";" _fields "${_row}")
      list(GET _fields 2 _artifact)
      list(APPEND _artifacts "${_root}/${_artifact}")
    endforeach()
    add_custom_target(${target} ALL DEPENDS ${_artifacts})
    set(HL_COMPAT_ARTIFACTS "${_artifacts}" PARENT_SCOPE)
    return()
  endif()

  if(NOT HL_CORPUS_MODE STREQUAL "REBUILD")
    message(FATAL_ERROR "HL_CORPUS_MODE must be PREBUILT or REBUILD")
  endif()
  find_program(_arm_cc aarch64-linux-gnu-gcc REQUIRED)
  find_program(_x86_cc x86_64-linux-gnu-gcc REQUIRED)
  file(STRINGS "${_root}/manifest.tsv" _cases)
  file(STRINGS "${_root}/recipe.tsv" _recipes)
  list(GET _recipes 1 _recipe)
  string(REPLACE "\t" ";" _recipe_fields "${_recipe}")
  list(GET _recipe_fields 1 _flags_text)
  separate_arguments(_flags UNIX_COMMAND "${_flags_text}")
  set(_outputs "")
  foreach(_row IN LISTS _cases)
    if(_row MATCHES "^#")
      continue()
    endif()
    string(REPLACE "\t" ";" _fields "${_row}")
    list(GET _fields 0 _case)
    list(GET _fields 1 _source)
    foreach(_isa aarch64 x86_64)
      if(_isa STREQUAL "aarch64")
        set(_cc "${_arm_cc}")
      else()
        set(_cc "${_x86_cc}")
      endif()
      set(_output "${CMAKE_CURRENT_BINARY_DIR}/smoke/${_isa}/${_case}")
      set(_expected "${_root}/prebuilt/${_isa}/${_case}")
      add_custom_command(
        OUTPUT "${_output}"
        COMMAND "${CMAKE_COMMAND}" -E make_directory "${CMAKE_CURRENT_BINARY_DIR}/smoke/${_isa}"
        COMMAND "${_cc}" ${_flags} -I"${_root}/source"
                "${_root}/${_source}" -o "${_output}"
        COMMAND "${CMAKE_COMMAND}" -DGENERATED=${_output} -DEXPECTED=${_expected}
                -P "${_HL_CORPUS_CMAKE_DIR}/Compare.cmake"
        DEPENDS "${_root}/${_source}" "${_root}/source/abi.h" "${_root}/recipe.tsv"
        VERBATIM COMMAND_EXPAND_LISTS)
      list(APPEND _outputs "${_output}")
    endforeach()
  endforeach()
  add_custom_target(${target} ALL DEPENDS ${_outputs})
  set(HL_COMPAT_ARTIFACTS "${_outputs}" PARENT_SCOPE)
endfunction()
