#!/usr/bin/env bash
set -euo pipefail

[[ -n "${OPUS_PREFIX:-}" ]] || { echo "OPUS_PREFIX is required" >&2; exit 1; }

mode=exists
for argument in "$@"; do
  case "$argument" in
    --cflags) mode=cflags ;;
    --libs) mode=libs ;;
    --modversion) mode=version ;;
    --exists|--print-errors|--silence-errors|--static) ;;
    opus|opus\>*) ;;
    \>=|[0-9]*) ;;
    *) ;;
  esac
done

case "$mode" in
  cflags) echo "-I$OPUS_PREFIX/include/opus" ;;
  libs) echo "-L$OPUS_PREFIX/lib -lopus -lm" ;;
  version) echo "1.6.1" ;;
  exists) exit 0 ;;
esac
