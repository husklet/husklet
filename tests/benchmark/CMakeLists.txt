cmake_minimum_required(VERSION 3.24)
project(hl_benchmark_guests LANGUAGES NONE)

set(HL_BENCH_AARCH64_CC "" CACHE FILEPATH "AArch64 Linux static compiler")
set(HL_BENCH_X86_64_CC "" CACHE FILEPATH "x86-64 Linux static compiler")

function(hl_benchmark isa compiler name source)
  if(NOT compiler)
    return()
  endif()
  set(output "${CMAKE_CURRENT_BINARY_DIR}/${isa}/${name}")
  add_custom_command(
    OUTPUT "${output}"
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${CMAKE_CURRENT_BINARY_DIR}/${isa}"
    COMMAND "${compiler}" -static-pie -O2 -std=gnu11 ${ARGN}
            "${CMAKE_CURRENT_SOURCE_DIR}/src/${source}" -o "${output}"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/src/${source}"
    VERBATIM)
  set_property(GLOBAL APPEND PROPERTY HL_BENCH_OUTPUTS "${output}")
endfunction()

foreach(isa aarch64 x86_64)
  string(TOUPPER "${isa}" upper)
  set(compiler "${HL_BENCH_${upper}_CC}")
  hl_benchmark(${isa} "${compiler}" combined-bench combined.c)
  hl_benchmark(${isa} "${compiler}" syscall syscall.c)
  hl_benchmark(${isa} "${compiler}" translate translate.c)
  hl_benchmark(${isa} "${compiler}" resource resource.c -pthread)
  foreach(pair event:4 file:2 ipc-latency:5 ipc-throughput:6 mmap:1 pipe:3)
    string(REPLACE ":" ";" fields "${pair}")
    list(GET fields 0 name)
    list(GET fields 1 operation)
    hl_benchmark(${isa} "${compiler}" "${name}" ops.c "-DHL_PERF_OP=${operation}")
  endforeach()
endforeach()

get_property(outputs GLOBAL PROPERTY HL_BENCH_OUTPUTS)
if(outputs)
  add_custom_target(benchmark-guests ALL DEPENDS ${outputs})
else()
  message(STATUS "Set HL_BENCH_AARCH64_CC and/or HL_BENCH_X86_64_CC to rebuild guests")
endif()
