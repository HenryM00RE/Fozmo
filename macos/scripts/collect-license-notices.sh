#!/usr/bin/env bash
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/common.sh"

require_command cargo
require_command curl
require_command node
require_command python3
require_command shasum

OUTPUT="${1:-$BUILD_DIR/third-party-notices}"
WORK="$BUILD_DIR/license-work"
SPDX_REVISION=c4a7237ec8f4654e867546f9f409749300f1bf4c
SPDX_SHA256=eb5a9dd08cef439e6fd03ebb7c0a69bb0249c492252f5b2de88e35c584332a32
SPDX_ARCHIVE="$WORK/spdx-license-list-$SPDX_REVISION.tar.gz"
SPDX_ROOT="$WORK/license-list-data-$SPDX_REVISION"
CORE_FEATURES=local_library,qobuz,pcm_output,airplay_helper,sonos,hegel,upnp,experimental_dsd256

rm -rf "$OUTPUT" "$WORK"
mkdir -p "$OUTPUT" "$WORK"

note "Resolving production Cargo and npm license metadata"
cargo metadata --locked --format-version 1 >"$WORK/root-metadata.json"
cargo metadata --locked --manifest-path "$ROOT_DIR/airplay-helper/Cargo.toml" --format-version 1 \
  >"$WORK/helper-metadata.json"
cargo tree --locked \
  --target aarch64-apple-darwin \
  --edges normal \
  --no-default-features \
  --features "$CORE_FEATURES" \
  --prefix none \
  --format '{p}' | LC_ALL=C sort -u >"$WORK/root-production.txt"
cargo tree --locked \
  --target aarch64-apple-darwin \
  --edges normal \
  --manifest-path "$ROOT_DIR/airplay-helper/Cargo.toml" \
  --prefix none \
  --format '{p}' | LC_ALL=C sort -u >"$WORK/helper-production.txt"

curl --fail --location --retry 3 --show-error \
  "https://codeload.github.com/spdx/license-list-data/tar.gz/$SPDX_REVISION" \
  -o "$SPDX_ARCHIVE"
[[ "$(shasum -a 256 "$SPDX_ARCHIVE" | awk '{print $1}')" == "$SPDX_SHA256" ]] \
  || die "SPDX license-list-data checksum mismatch"
tar -xzf "$SPDX_ARCHIVE" -C "$WORK"

python3 - \
  "$ROOT_DIR" \
  "$WORK/root-metadata.json" \
  "$WORK/helper-metadata.json" \
  "$WORK/root-production.txt" \
  "$WORK/helper-production.txt" \
  "$SPDX_ROOT/text" \
  "$ROOT_DIR/ui/package-lock.json" \
  "$OUTPUT" <<'PY'
import hashlib
import json
import pathlib
import re
import shutil
import sys

(
    root_arg,
    root_metadata_arg,
    helper_metadata_arg,
    root_tree_arg,
    helper_tree_arg,
    spdx_text_arg,
    npm_lock_arg,
    output_arg,
) = sys.argv[1:]

root = pathlib.Path(root_arg).resolve()
output = pathlib.Path(output_arg)
spdx_text = pathlib.Path(spdx_text_arg)
license_name = re.compile(r"^(license|licence|copying|notice|unlicense|copyright)", re.I)
package_line = re.compile(r"^(.+) v([^ ]+)(?: \(.*\))?$")
spdx_token = re.compile(r"[A-Za-z0-9][A-Za-z0-9.+-]*")
operators = {"AND", "OR", "WITH"}


def safe(value):
    return re.sub(r"[^A-Za-z0-9._+-]", "_", value)


def copy_license_files(package, destination):
    manifest_dir = pathlib.Path(package["manifest_path"]).parent
    candidates = []
    for path in manifest_dir.rglob("*"):
        if not path.is_file():
            continue
        relative = path.relative_to(manifest_dir)
        if len(relative.parts) > 3:
            continue
        if license_name.match(path.name) or any(part.lower() in {"licenses", "license"} for part in relative.parts[:-1]):
            candidates.append(path)

    license_file = package.get("license_file")
    if license_file:
        candidate = (manifest_dir / license_file).resolve()
        if candidate.is_file():
            candidates.append(candidate)

    source = package.get("source") or ""
    if source.startswith("git+"):
        ancestor = manifest_dir
        for _ in range(6):
            if (ancestor / ".git").exists():
                candidates.extend(path for path in ancestor.iterdir() if path.is_file() and license_name.match(path.name))
                break
            ancestor = ancestor.parent

    copied = []
    seen_hashes = set()
    for source_path in sorted(set(candidates), key=lambda path: str(path)):
        digest = hashlib.sha256(source_path.read_bytes()).hexdigest()
        if digest in seen_hashes:
            continue
        seen_hashes.add(digest)
        try:
            relative = source_path.relative_to(manifest_dir)
            name = "__".join(relative.parts)
        except ValueError:
            name = source_path.name
        destination_path = destination / safe(name)
        shutil.copyfile(source_path, destination_path)
        copied.append(destination_path.name)
    return copied


metadata_documents = [json.load(open(root_metadata_arg)), json.load(open(helper_metadata_arg))]
packages_by_name_version = {}
workspace_ids = set()
for document in metadata_documents:
    workspace_ids.update(document["workspace_members"])
    for package in document["packages"]:
        packages_by_name_version.setdefault((package["name"], package["version"]), []).append(package)

wanted = set()
for tree_path in [root_tree_arg, helper_tree_arg]:
    for line in pathlib.Path(tree_path).read_text().splitlines():
        match = package_line.match(line.strip())
        if not match:
            raise SystemExit(f"unparseable cargo tree package line: {line}")
        wanted.add(match.groups())

cargo_root = output / "cargo"
cargo_root.mkdir(parents=True)
cargo_records = []
canonical_ids = set()
processed_ids = set()
for key in sorted(wanted):
    matches = packages_by_name_version.get(key, [])
    if not matches:
        raise SystemExit(f"cargo metadata missing resolved package {key[0]} {key[1]}")
    for package in matches:
        if package["id"] in processed_ids or package["id"] in workspace_ids:
            continue
        processed_ids.add(package["id"])
        declared = package.get("license")
        inherited = False
        source = package.get("source") or "path"
        if not declared and "github.com/Pabldi08/airplay2-rs" in source:
            declared = "GPL-2.0-only"
            inherited = True
        if not declared and not package.get("license_file"):
            raise SystemExit(f"resolved Cargo package lacks license declaration: {package['name']} {package['version']} ({source})")

        identity = f"{package['name']}-{package['version']}-{source}"
        directory = cargo_root / f"{safe(package['name'])}-{safe(package['version'])}-{hashlib.sha256(identity.encode()).hexdigest()[:10]}"
        directory.mkdir()
        copied = copy_license_files(package, directory)
        if not copied and not declared:
            raise SystemExit(f"resolved Cargo package has no license text: {package['name']} {package['version']}")
        if declared:
            canonical_ids.update(token for token in spdx_token.findall(declared) if token not in operators)
        cargo_records.append({
            "license": declared,
            "license_files": copied,
            "license_inherited_from_git_repository": inherited,
            "name": package["name"],
            "repository": package.get("repository"),
            "source": source,
            "version": package["version"],
        })

npm_lock = json.load(open(npm_lock_arg))
npm_root = output / "npm"
npm_root.mkdir()
npm_records = []
ui_root = root / "ui"
for lock_path, package in sorted(npm_lock["packages"].items()):
    if not lock_path.startswith("node_modules/") or package.get("dev"):
        continue
    name = lock_path[len("node_modules/"):]
    declared = package.get("license")
    if not declared:
        raise SystemExit(f"resolved npm package lacks license declaration: {name} {package.get('version')}")
    package_dir = ui_root / lock_path
    candidates = sorted(path for path in package_dir.iterdir() if path.is_file() and license_name.match(path.name))
    if not candidates:
        raise SystemExit(f"resolved npm package lacks license text: {name} {package.get('version')}")
    identity = f"{name}-{package.get('version')}-{package.get('resolved')}"
    directory = npm_root / f"{safe(name)}-{safe(package.get('version', 'unknown'))}-{hashlib.sha256(identity.encode()).hexdigest()[:10]}"
    directory.mkdir()
    copied = []
    for source_path in candidates:
        destination = directory / safe(source_path.name)
        shutil.copyfile(source_path, destination)
        copied.append(destination.name)
    canonical_ids.update(token for token in spdx_token.findall(declared) if token not in operators)
    npm_records.append({
        "integrity": package.get("integrity"),
        "license": declared,
        "license_files": copied,
        "name": name,
        "source": package.get("resolved"),
        "version": package.get("version"),
    })

canonical_root = output / "spdx-text"
canonical_root.mkdir()
for identifier in sorted(canonical_ids):
    source_path = spdx_text / f"{identifier}.txt"
    if not source_path.is_file():
        raise SystemExit(f"SPDX canonical license/exception text is missing: {identifier}")
    shutil.copyfile(source_path, canonical_root / source_path.name)

(output / "cargo-packages.json").write_text(json.dumps(cargo_records, indent=2, sort_keys=True) + "\n")
(output / "npm-packages.json").write_text(json.dumps(npm_records, indent=2, sort_keys=True) + "\n")
(output / "README.txt").write_text(
    "Fozmo third-party dependency notices\n\n"
    f"Cargo production packages: {len(cargo_records)}\n"
    f"npm production packages: {len(npm_records)}\n"
    "Each record includes resolved name, version, SPDX declaration, source, and copied notice files.\n"
    "Canonical SPDX texts are from license-list-data v3.28.0, commit c4a7237ec8f4654e867546f9f409749300f1bf4c.\n"
)

for path in output.rglob("*"):
    if path.is_file():
        path.chmod(0o644)
PY

find "$OUTPUT" -exec touch -h -t 200001010000 {} +
note "Collected deterministic dependency notices at $OUTPUT"
