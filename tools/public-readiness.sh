#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "error: $*" >&2
  exit 1
}

command -v rg >/dev/null 2>&1 || fail "ripgrep (rg) is required for public-readiness scans"

echo "==> Checking tracked public-file policy"
./tools/check-tracked-public-files.sh

echo "==> Checking ignored local runtime data"
ignored_runtime="$(git status --ignored --short \
  .fozmo.lock \
  install.json \
  backups \
  logs \
  secrets.dev.json \
  settings.json \
  settings.json.bak \
  settings.json.recovery-required \
  ui/library \
  library \
  docs/manual-smoke-evidence.local.md \
  docs/screenshots/local \
  docs/screenshots/private \
  2>/dev/null || true)"
if [[ -n "$ignored_runtime" ]]; then
  echo "$ignored_runtime" | sed 's/^/    /'
fi

echo "==> Validating settings example"
node -e "JSON.parse(require('fs').readFileSync('settings.example.json','utf8'))" \
  || fail "settings.example.json is not valid JSON"

echo "==> Checking settings example stays sanitized"
if rg -n --max-columns 240 '(/Users/admin|RoonMounts|192\.168\.1\.166|pairing_tokens":|pairing_token_records":\s*\[\s*\{|device_name":\s*"|host":\s*")' settings.example.json; then
  fail "settings.example.json contains local-looking data"
fi

echo "==> Checking generated frontend snapshot is fresh"
./tools/check-frontend-snapshot.sh --require-clean

echo "==> Running general secret scan"
./tools/secret-scan.sh

echo "==> Checking old app identity strings are gone"
old_identity_matches="$(rg -n --max-columns 240 \
  'UPSAMPLER_|x-upsampler|upsamplerPairingToken|upsampler-ui|upsampler\.local|_upsampler|UpsamplerApiContract|(^|[^[:alnum:]_])Upsampler([^[:alnum:]_]|$)|(^|[^[:alnum:]_])upsampler([^[:alnum:]_]|$)' \
  . \
  --hidden \
  --glob '!target/**' \
  --glob '!node_modules/**' \
  --glob '!static/react-app/assets/**' \
  --glob '!ui/src/shared/generated/**' \
  --glob '!src/audio/**' \
  --glob '!audio_tests/**' \
  --glob '!docs/audio-pipeline.md' \
  --glob '!docs/dsp.md' \
  --glob '!tools/public-readiness.sh' \
  --glob '!.git/**' \
  2>/dev/null || true)"
if [[ -n "$old_identity_matches" ]]; then
  echo "$old_identity_matches" >&2
  fail "old Upsampler identity strings remain outside allowed DSP terminology"
fi

echo "==> Checking private identity strings across hidden and visible files"
private_identity_matches="$(rg -n --hidden --max-columns 240 \
  '(/Users/admin|/RoonMounts/|192\.168\.1\.166|80fOw|Profile2)' \
  . \
  --glob '!.git/**' \
  --glob '!target/**' \
  --glob '!node_modules/**' \
  --glob '!tools/public-readiness.sh' \
  2>/dev/null || true)"
if [[ -n "$private_identity_matches" ]]; then
  echo "$private_identity_matches" >&2
  fail "private identity strings remain in the public snapshot"
fi

echo "==> Checking public playback terminology"
playback_term_matches="$(rg -n --max-columns 240 \
  'Signal path|Playback zone|No enabled zones found|Hegel zone|Choose zone|zone plays|Hegel zone will' \
  ui/src docs \
  --glob '*.ts' \
  --glob '*.tsx' \
  --glob '*.md' \
  2>/dev/null || true)"
if [[ -n "$playback_term_matches" ]]; then
  echo "$playback_term_matches" >&2
  fail "old playback terminology remains in public UI/docs copy"
fi

echo "==> Scanning generated frontend assets for local data"
if rg -n --hidden --max-columns 240 '(/Users/admin|/RoonMounts/|192\.168\.1\.166|80fOw|Profile2)' static/react-app; then
  fail "generated frontend assets contain local-looking data"
fi

echo "==> Public-readiness checks passed"
