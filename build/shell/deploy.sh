#!/bin/sh
# Copy a cross-built binary (default: rtshell, release) plus run.sh to ~/rtshell/ on the Surface.
# Build first: build/shell/build.sh shell --release
#   ./deploy.sh [path/to/binary]
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
HOST=${SURFACE:-alarm@surface-rt}
BIN=${1:-$HERE/../../shell/target/armv7-unknown-linux-gnueabihf/release/rtshell}
ssh "$HOST" 'mkdir -p ~/rtshell'
# Strip a copy (debug info is ~2/3 of the file); keep the unstripped one on the host for gdb/addr2line.
llvm-strip -o "$HERE/.deploy-$(basename "$BIN")" "$BIN"
scp -q "$HERE/.deploy-$(basename "$BIN")" "$HOST:rtshell/$(basename "$BIN").new"
scp -q "$HERE/run.sh" "$HOST:rtshell/run.sh"
ssh "$HOST" "cd ~/rtshell && mv $(basename "$BIN").new $(basename "$BIN") && chmod +x run.sh $(basename "$BIN") && ls -la"
rm -f "$HERE/.deploy-$(basename "$BIN")"
