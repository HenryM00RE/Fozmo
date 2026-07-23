//! Embed the compiler and source identity in every Fozmo executable.
//!
//! These values describe the binary that was compiled, rather than the shell
//! environment present when it is later launched. Measurement tools can compare
//! the embedded source digest with a digest of the current checkout to reject a
//! stale executable and can inspect the embedded profile/target CPU directly.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const PROVENANCE_SCHEMA: &str = "fozmo-build-provenance-v1";
const SOURCE_SNAPSHOT_SCHEMA: &str = "fozmo-dsd-public-source-snapshot-v2";

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo must set CARGO_MANIFEST_DIR"),
    );

    build_apple_music_process_tap(&manifest_dir);
    emit_rerun_inputs(&manifest_dir);
    emit("FOZMO_BUILD_PROVENANCE_SCHEMA", PROVENANCE_SCHEMA);
    emit("FOZMO_BUILD_SOURCE_SNAPSHOT_SCHEMA", SOURCE_SNAPSHOT_SCHEMA);
    emit_from_env("FOZMO_BUILD_PROFILE", "PROFILE");
    emit_from_env("FOZMO_BUILD_OPT_LEVEL", "OPT_LEVEL");
    emit_from_env("FOZMO_BUILD_DEBUG_INFO", "DEBUG");
    emit(
        "FOZMO_BUILD_DEBUG_ASSERTIONS",
        bool_string(env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some()),
    );
    emit_from_env("FOZMO_BUILD_HOST", "HOST");
    emit_from_env("FOZMO_BUILD_TARGET", "TARGET");
    emit_from_env("FOZMO_BUILD_PANIC_STRATEGY", "CARGO_CFG_PANIC");

    let encoded_rustflags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let rustflag_args = encoded_rustflags
        .split('\x1f')
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let target_cpu = effective_target_cpu(&rustflag_args).unwrap_or("unspecified");
    emit(
        "FOZMO_BUILD_ENCODED_RUSTFLAGS_HEX",
        &hex(encoded_rustflags.as_bytes()),
    );
    emit(
        "FOZMO_BUILD_RUSTFLAGS_DISPLAY",
        &single_line(&rustflag_args.join(" ")),
    );
    emit("FOZMO_BUILD_TARGET_CPU", target_cpu);
    emit(
        "FOZMO_BUILD_NATIVE_CPU_REQUESTED",
        bool_string(target_cpu == "native"),
    );

    let target_features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    emit(
        "FOZMO_BUILD_TARGET_FEATURES",
        &single_line(&target_features),
    );
    emit(
        "FOZMO_BUILD_TARGET_FEATURES_HEX",
        &hex(target_features.as_bytes()),
    );
    emit("FOZMO_BUILD_CARGO_FEATURES", &enabled_cargo_features());

    emit_rustc_identity();
    emit_git_identity(&manifest_dir);
    emit(
        "FOZMO_BUILD_SOURCE_SNAPSHOT_SHA256",
        &source_snapshot_sha256(&manifest_dir).unwrap_or_else(|| "unavailable".to_string()),
    );
}

fn build_apple_music_process_tap(manifest_dir: &Path) {
    if env::var_os("CARGO_FEATURE_APPLE_MUSIC_MUSICKIT").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
    {
        return;
    }

    let bridge = manifest_dir.join("src/services/apple_music_musickit/process_tap_bridge.m");
    let embedded_info = manifest_dir.join("macos/FozmoServer-Info.plist");
    println!("cargo:rerun-if-changed={}", bridge.display());
    println!("cargo:rerun-if-changed={}", embedded_info.display());

    cc::Build::new()
        .file(&bridge)
        .flag("-fobjc-arc")
        .flag("-mmacosx-version-min=11.0")
        .compile("fozmo_process_tap_bridge");

    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!(
        "cargo:rustc-link-arg-bin=fozmo=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        embedded_info.display()
    );
}

fn emit_rerun_inputs(manifest_dir: &Path) {
    for variable in [
        "RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    for relative in source_snapshot_files(manifest_dir).unwrap_or_default() {
        println!("cargo:rerun-if-changed={}", relative.display());
    }
    emit_git_rerun_inputs(manifest_dir);
}

fn emit_git_rerun_inputs(manifest_dir: &Path) {
    let dot_git = manifest_dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        Some(dot_git)
    } else {
        fs::read_to_string(&dot_git).ok().and_then(|contents| {
            contents
                .trim()
                .strip_prefix("gitdir:")
                .map(str::trim)
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        manifest_dir.join(path)
                    }
                })
        })
    };
    let Some(git_dir) = git_dir else {
        return;
    };

    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
    if let Ok(contents) = fs::read_to_string(head)
        && let Some(reference) = contents.trim().strip_prefix("ref:").map(str::trim)
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference).display()
        );
    }
}

fn emit_rustc_identity() {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    emit("FOZMO_BUILD_RUSTC_PATH", &single_line(&rustc));
    emit(
        "FOZMO_BUILD_RUSTC_WRAPPER",
        &single_line(&env::var("RUSTC_WRAPPER").unwrap_or_default()),
    );
    emit(
        "FOZMO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        &single_line(&env::var("RUSTC_WORKSPACE_WRAPPER").unwrap_or_default()),
    );

    match Command::new(&rustc).arg("-vV").output() {
        Ok(output) if output.status.success() => {
            emit(
                "FOZMO_BUILD_RUSTC_VERBOSE_HEX",
                &hex(output.stdout.as_slice()),
            );
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("unavailable")
                .to_string();
            emit("FOZMO_BUILD_RUSTC_VERSION", &single_line(&version));
        }
        _ => {
            emit("FOZMO_BUILD_RUSTC_VERBOSE_HEX", "");
            emit("FOZMO_BUILD_RUSTC_VERSION", "unavailable");
        }
    }
}

fn emit_git_identity(manifest_dir: &Path) {
    let commit = command_stdout(manifest_dir, "git", &["rev-parse", "--verify", "HEAD"])
        .unwrap_or_else(|| "unavailable".to_string());
    emit("FOZMO_BUILD_GIT_COMMIT", &single_line(&commit));

    let dirty = Command::new("git")
        .current_dir(manifest_dir)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| bool_string(!output.stdout.is_empty()))
        .unwrap_or("unknown");
    emit("FOZMO_BUILD_GIT_DIRTY", dirty);
}

fn command_stdout(directory: &Path, program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn source_snapshot_sha256(manifest_dir: &Path) -> Option<String> {
    // Keep this byte-for-byte compatible with the public DSD bench's runtime
    // source_snapshot_sha256() implementation. The runtime/build comparison is
    // what makes a directly launched stale binary detectable.
    let relative_files = source_snapshot_files(manifest_dir)?;
    let mut digest = Sha256::new();
    digest.update(b"fozmo-dsd-public-source-snapshot-v2\0");
    for relative in relative_files {
        let path = relative.to_string_lossy();
        let bytes = fs::read(manifest_dir.join(&relative)).ok()?;
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn source_snapshot_files(manifest_dir: &Path) -> Option<Vec<PathBuf>> {
    let mut relative_files = vec![
        PathBuf::from("Cargo.toml"),
        PathBuf::from("Cargo.lock"),
        PathBuf::from("build.rs"),
        PathBuf::from("src/lib.rs"),
        PathBuf::from("audio_tests/dsd_public_quality.rs"),
        PathBuf::from("audio_tests/dsd_public/analysis.rs"),
        PathBuf::from("audio_tests/dsd_public/signals.rs"),
    ];
    let mut directories = vec![PathBuf::from("src/audio")];
    while let Some(relative_directory) = directories.pop() {
        for entry in fs::read_dir(manifest_dir.join(&relative_directory)).ok()? {
            let entry = entry.ok()?;
            let absolute = entry.path();
            let relative = absolute.strip_prefix(manifest_dir).ok()?.to_path_buf();
            if absolute.is_dir() {
                directories.push(relative);
            } else if absolute.extension() == Some(OsStr::new("rs")) {
                relative_files.push(relative);
            }
        }
    }
    relative_files.sort();
    relative_files.dedup();
    Some(relative_files)
}

fn effective_target_cpu<'a>(arguments: &'a [&'a str]) -> Option<&'a str> {
    let mut target_cpu = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index];
        if matches!(argument, "-C" | "--codegen") {
            if let Some(value) = arguments
                .get(index + 1)
                .and_then(|value| codegen_target_cpu(value))
            {
                target_cpu = Some(value);
            }
            index += 2;
            continue;
        }
        if let Some(value) = argument
            .strip_prefix("-C")
            .and_then(codegen_target_cpu)
            .or_else(|| {
                argument
                    .strip_prefix("--codegen=")
                    .and_then(codegen_target_cpu)
            })
        {
            target_cpu = Some(value);
        }
        index += 1;
    }
    target_cpu
}

fn codegen_target_cpu(value: &str) -> Option<&str> {
    value
        .strip_prefix("target-cpu=")
        .filter(|cpu| !cpu.is_empty())
}

fn enabled_cargo_features() -> String {
    let mut features = env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter_map(|name| name.strip_prefix("CARGO_FEATURE_").map(str::to_owned))
        .map(|name| name.to_ascii_lowercase().replace('_', "-"))
        .collect::<Vec<_>>();
    features.sort();
    features.dedup();
    features.join(",")
}

fn emit_from_env(destination: &str, source: &str) {
    let value = env::var(source).unwrap_or_else(|_| "unavailable".to_string());
    emit(destination, &single_line(&value));
}

fn emit(name: &str, value: &str) {
    println!("cargo:rustc-env={name}={value}");
}

fn bool_string(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_target_cpu_codegen_option_wins() {
        assert_eq!(
            effective_target_cpu(&["-C", "target-cpu=generic", "-Ctarget-cpu=native",]),
            Some("native")
        );
        assert_eq!(
            effective_target_cpu(&["--codegen=target-cpu=native", "-C", "target-cpu=apple-m1",]),
            Some("apple-m1")
        );
        assert_eq!(effective_target_cpu(&["-C", "opt-level=3"]), None);
    }

    #[test]
    fn byte_encoding_is_stable() {
        assert_eq!(hex(&[0x00, 0x1f, 0xa5, 0xff]), "001fa5ff");
    }
}
