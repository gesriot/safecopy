#!/bin/bash
set -euo pipefail

native_source=$1
native_output=$2
java_runtime=$3
clang_bin=$(command -v clang++)
linker_bin=$(command -v ld.lld)
tool_prefix=$(dirname "$(dirname "$clang_bin")")
clang_resource=$($clang_bin --print-resource-dir)
native_object="${native_output}.o"

mkdir -p "$(dirname "$native_output")"

"$clang_bin" \
    -c -fPIC -O2 -fno-exceptions -fno-rtti -std=c++17 \
    -Wall -Wextra -Werror \
    -I"$java_runtime/include" \
    -I"$java_runtime/include/linux" \
    "$native_source" \
    -o "$native_object"

"$linker_bin" \
    --sysroot="$tool_prefix" \
    -EL --fix-cortex-a53-843419 \
    -z now -z relro -z max-page-size=16384 \
    --no-rosegment --hash-style=gnu --eh-frame-hdr \
    -m aarch64linux -shared \
    -soname libsafecopy_io.so \
    -o "$native_output" \
    "$tool_prefix/lib/crtbegin_so.o" \
    -L"$tool_prefix/lib" \
    -L"$tool_prefix/aarch64-linux-android/lib" \
    -L/system/lib64 \
    --no-undefined \
    "$native_object" \
    "$clang_resource/lib/linux/libclang_rt.builtins-aarch64-android.a" \
    -l:libunwind.a -ldl -lm -lc \
    "$tool_prefix/lib/crtend_so.o"

rm -f "$native_object"
