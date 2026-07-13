#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
git -C "$ROOT_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
  || { echo "error: not a Git worktree: $ROOT_DIR" >&2; exit 1; }

failed=0
reject() {
  printf 'forbidden tracked file (%s): %s\n' "$1" "$2" >&2
  failed=1
}

while IFS= read -r -d '' path; do
  case "$path" in
    settings.example.json)
      continue
      ;;
    static/react-app/index.html)
      continue
      ;;
    static/react-app/assets/*)
      if [[ "$path" =~ ^static/react-app/assets/[A-Za-z0-9._-]+-[A-Za-z0-9_-]{8,}\.(css|js|js\.map|png|jpe?g|svg|webp|woff2?|ttf)$ ]]; then
        continue
      fi
      reject "generated frontend asset without a content hash" "$path"
      continue
      ;;
    static/react-app/*)
      reject "unexpected generated frontend path" "$path"
      continue
      ;;
  esac

  case "$path" in
    .DS_Store|*/.DS_Store)
      reject "Finder metadata" "$path"
      ;;
    settings*.json|*/settings*.json|\
    settings.json.bak|*/settings.json.bak|\
    settings.json.recovery-required|*/settings.json.recovery-required|\
    settings.json.corrupt-*|*/settings.json.corrupt-*)
      reject "settings/runtime state" "$path"
      ;;
    *.db|*.sqlite|*.sqlite3|*.log)
      reject "runtime database or log" "$path"
      ;;
    *.pem|*.p12|*.key)
      reject "credential or private-key material; add only a reviewed exact-path exception" "$path"
      ;;
    *.app|*.app/*|*.driver|*.driver/*|*.dmg|*.pkg)
      reject "generated application/package output" "$path"
      ;;
    drivers/fozmo-capture/build|drivers/fozmo-capture/build/*|\
    macos/build|macos/build/*|\
    macos/FozmoLauncher/.build|macos/FozmoLauncher/.build/*|\
    target|target/*|*/target|*/target/*|\
    dist|dist/*|ui/dist|ui/dist/*|\
    ui/node_modules|ui/node_modules/*|ui/.vite|ui/.vite/*|\
    audio_tests/out|audio_tests/out/*)
      reject "generated build output" "$path"
      ;;
    .fozmo.lock|*/.fozmo.lock|\
    install.json|*/install.json|\
    secrets.dev.json|*/secrets.dev.json)
      reject "runtime metadata or development secrets" "$path"
      ;;
    backups|backups/*|*/backups|*/backups/*|\
    logs|logs/*|*/logs|*/logs/*|\
    music|music/*|library|library/*|cache|cache/*|\
    ui/music|ui/music/*|ui/library|ui/library/*|ui/cache|ui/cache/*|\
    static/user-fonts|static/user-fonts/*)
      reject "runtime-owned directory" "$path"
      ;;
  esac
done < <(git -C "$ROOT_DIR" ls-files -z)

if git -C "$ROOT_DIR" ls-files --error-unmatch static/react-app/index.html >/dev/null 2>&1; then
  while IFS= read -r reference; do
    relative="${reference#/react-app/}"
    if ! git -C "$ROOT_DIR" ls-files --error-unmatch "static/react-app/$relative" >/dev/null 2>&1; then
      reject "frontend index references a missing asset" "static/react-app/$relative"
    fi
  done < <(rg -o '[/]react-app/assets/[A-Za-z0-9._-]+' \
    "$ROOT_DIR/static/react-app/index.html" | sort -u)

  while IFS= read -r asset; do
    basename="${asset##*/}"
    if ! git -C "$ROOT_DIR" grep -Fq -- "$basename" -- static/react-app; then
      reject "unreferenced generated frontend asset" "$asset"
    fi
  done < <(git -C "$ROOT_DIR" ls-files 'static/react-app/assets/*')
fi

if [[ "$failed" == "1" ]]; then
  echo "error: tracked public-file policy failed" >&2
  exit 1
fi

echo "==> Tracked public-file policy passed"
