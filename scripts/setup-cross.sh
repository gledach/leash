#!/bin/sh
# Prepare libgcc/libgcc_eh stand-ins so rustc's x86_64-pc-windows-gnu
# target can link with llvm-mingw (which ships compiler-rt + libunwind).
set -e
ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/.cargo/mingw-gcc-compat"
mkdir -p "$OUT"
if [ -z "$LLVM_MINGW" ]; then
  echo "Set LLVM_MINGW to your llvm-mingw prefix (the folder that contains bin/x86_64-w64-mingw32-gcc)." >&2
  exit 1
fi
# compiler-rt builtins stand in for libgcc; libunwind stands in for libgcc_eh
BUILTIN=$(find "$LLVM_MINGW"/lib/clang -name 'libclang_rt.builtins-x86_64.a' | head -n1)
UNWIND="$LLVM_MINGW/x86_64-w64-mingw32/lib/libunwind.a"
if [ ! -f "$BUILTIN" ] || [ ! -f "$UNWIND" ]; then
  echo "could not find compiler-rt builtins or libunwind under $LLVM_MINGW" >&2
  exit 1
fi
cp "$BUILTIN" "$OUT/libgcc.a"
cp "$UNWIND" "$OUT/libgcc_eh.a"
echo "wrote $OUT/libgcc.a and libgcc_eh.a"
