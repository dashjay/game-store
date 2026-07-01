cmake_minimum_required(VERSION 3.16)
project(gamestore_spike_cpp CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
set(CMAKE_CXX_FLAGS_RELEASE "-O2")

# The default `c++` alternative on this image is clang without working libstdc++
# include paths; prefer g++ which finds the C++ standard library. Override with
# -DCMAKE_CXX_COMPILER=... if you have a working clang toolchain.
if(NOT DEFINED CMAKE_CXX_COMPILER)
  find_program(GXX g++)
  if(GXX)
    set(CMAKE_CXX_COMPILER ${GXX})
  endif()
endif()

add_executable(gamestore_spike
  src/main.cpp
  src/storage.cpp
  src/resp.cpp
  src/commands.cpp
)

target_include_directories(gamestore_spike PRIVATE src)

# Link the system RocksDB (librocksdb-dev). This is the C++-native path: no FFI
# layer, exactly the integration story described in the language-selection plan.
find_library(ROCKSDB_LIB rocksdb REQUIRED)
target_link_libraries(gamestore_spike
  ${ROCKSDB_LIB}
  pthread
)
