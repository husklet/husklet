#----------------------------------------------------------------
# Generated CMake target import file for configuration "None".
#----------------------------------------------------------------

# Commands may need to know the format version.
set(CMAKE_IMPORT_FILE_VERSION 1)

# Import target "Imath::Imath" for configuration "None"
set_property(TARGET Imath::Imath APPEND PROPERTY IMPORTED_CONFIGURATIONS NONE)
set_target_properties(Imath::Imath PROPERTIES
  IMPORTED_LOCATION_NONE "${_IMPORT_PREFIX}/lib/aarch64-linux-gnu/libImath-3_1.so.29.5.0"
  IMPORTED_SONAME_NONE "libImath-3_1.so.29"
  )

list(APPEND _cmake_import_check_targets Imath::Imath )
list(APPEND _cmake_import_check_files_for_Imath::Imath "${_IMPORT_PREFIX}/lib/aarch64-linux-gnu/libImath-3_1.so.29.5.0" )

# Import target "Imath::PyImath_Python3_11" for configuration "None"
set_property(TARGET Imath::PyImath_Python3_11 APPEND PROPERTY IMPORTED_CONFIGURATIONS NONE)
set_target_properties(Imath::PyImath_Python3_11 PROPERTIES
  IMPORTED_LINK_DEPENDENT_LIBRARIES_NONE "Python3::Python"
  IMPORTED_LOCATION_NONE "${_IMPORT_PREFIX}/lib/aarch64-linux-gnu/libPyImath_Python3_11-3_1.so.29.5.0"
  IMPORTED_SONAME_NONE "libPyImath_Python3_11-3_1.so.29"
  )

list(APPEND _cmake_import_check_targets Imath::PyImath_Python3_11 )
list(APPEND _cmake_import_check_files_for_Imath::PyImath_Python3_11 "${_IMPORT_PREFIX}/lib/aarch64-linux-gnu/libPyImath_Python3_11-3_1.so.29.5.0" )

# Commands beyond this point should not need to know the version.
set(CMAKE_IMPORT_FILE_VERSION)
