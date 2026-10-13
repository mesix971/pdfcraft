//! `cargo xtask android`: build the Android app (`apps/pdfcraft-android`) and package it as a
//! signed APK, with the Android NDK and SDK and no Gradle or Java code.
//!
//! 1. `cargo build -p pdfcraft-android --target <triple>` for each ABI, linked with the NDK's clang
//!    for API level [`MIN_SDK`], then stripped.
//! 2. `aapt2` compiles the launcher icons (`assets/app-icon`) and links them with
//!    `packaging/android/AndroidManifest.xml` and, when `cargo xtask models` has fetched them, the
//!    OCR models (`assets/models`, unpacked by the app on first start) into an APK.
//! 3. `aapt add` puts the libraries in `lib/<abi>/`, `zipalign` aligns it, `apksigner` signs it.
//!
//! Options: `--abi arm64-v8a|armeabi-v7a|x86_64|x86` (repeatable; default arm64-v8a),
//! `--all-abis`, `--debug` (a dev build), `--check` (only clippy, with `-D warnings`, the app and
//! the shell for each ABI, as CI does; needs no SDK). The APK goes to
//! `dist/android/pdfcraft-<version>-<abis>.apk`.
//!
//! Where the tools are: `ANDROID_NDK_HOME` (or `ANDROID_NDK_ROOT`, `ANDROID_NDK_LATEST_HOME`,
//! `NDK_HOME`) and `ANDROID_HOME` (or `ANDROID_SDK_ROOT`), whose newest `build-tools/` is used
//! with `platforms/android-<TARGET_SDK>` (or the newest platform). Signing: the keystore in
//! `PDFCRAFT_ANDROID_KEYSTORE` (password `PDFCRAFT_ANDROID_KEYSTORE_PASS`, alias
//! `PDFCRAFT_ANDROID_KEY_ALIAS`, default `pdfcraft`), else the standard debug keystore
//! (`~/.android/debug.keystore`, made with `keytool` when missing). A debug-signed APK installs
//! for testing; releases need a key of their own (apps/pdfcraft-android/README.md).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::gates::{root, target_dir};

/// The oldest Android the app runs on: 8.0 (API 26).
pub const MIN_SDK: u32 = 26;
/// The Android the app is built for (see packaging/android/AndroidManifest.xml on 35).
pub const TARGET_SDK: u32 = 34;
/// The name Android loads (`android.app.lib_name` in the manifest).
const LIB: &str = "libpdfcraft_android.so";

/// An Android ABI and the Rust target that builds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abi {
    pub name: &'static str,
    pub triple: &'static str,
    /// The NDK's clang prefix (`<prefix><api>-clang`).
    clang: &'static str,
}

pub const ABIS: [Abi; 4] = [
    Abi { name: "arm64-v8a", triple: "aarch64-linux-android", clang: "aarch64-linux-android" },
    Abi { name: "armeabi-v7a", triple: "armv7-linux-androideabi", clang: "armv7a-linux-androideabi" },
    Abi { name: "x86_64", triple: "x86_64-linux-android", clang: "x86_64-linux-android" },
    Abi { name: "x86", triple: "i686-linux-android", clang: "i686-linux-android" },
];

/// What the command line asked for.
#[derive(Debug, PartialEq)]
struct Options {
    abis: Vec<Abi>,
    debug: bool,
    check: bool,
}

fn parse(args: &[String]) -> anyhow::Result<Options> {
    let mut abis = Vec::new();
    let (mut debug, mut check) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--abi" => {
                let name = it.next().context("--abi needs a name (arm64-v8a, armeabi-v7a, x86_64 or x86)")?;
                let abi = ABIS.iter().find(|x| x.name == name).with_context(|| format!("unknown ABI {name}"))?;
                if !abis.contains(abi) {
                    abis.push(*abi);
                }
            }
            "--all-abis" => abis = ABIS.to_vec(),
            "--debug" => debug = true,
            "--check" => check = true,
            other => bail!("unknown option {other}"),
        }
    }
    if abis.is_empty() {
        abis.push(ABIS[0]);
    }
    Ok(Options { abis, debug, check })
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let options = parse(args)?;
    let ndk = ndk_dir()?;
    let clang_dir = ndk_bin(&ndk)?;
    for abi in &options.abis {
        build_lib(abi, &clang_dir, options.debug, options.check)?;
    }
    if options.check {
        println!("android: clippy clean for {} ABI(s)", options.abis.len());
        return Ok(());
    }
    let sdk = sdk_dir()?;
    let tools = newest(&sdk.join("build-tools"), |_| true).context("no build-tools in the Android SDK (sdkmanager \"build-tools;34.0.0\")")?;
    let platform = sdk.join("platforms").join(format!("android-{TARGET_SDK}"));
    let platform = if platform.is_dir() {
        platform
    } else {
        newest(&sdk.join("platforms"), |n| n.starts_with("android-"))
            .context("no platform in the Android SDK (sdkmanager \"platforms;android-34\")")?
    };
    let version = crate::version::read(&std::fs::read_to_string(root().join("Cargo.toml"))?).map_err(anyhow::Error::msg)?;
    let apk = package(&options, &clang_dir, &tools, &platform.join("android.jar"), &version)?;
    println!("android: {}", apk.display());
    Ok(())
}

/// The NDK, from the usual variables.
fn ndk_dir() -> anyhow::Result<PathBuf> {
    ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK_LATEST_HOME", "NDK_HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|d| d.is_dir())
        .context("set ANDROID_NDK_HOME to the Android NDK (r26 or later)")
}

/// The Android SDK, from the usual variables.
fn sdk_dir() -> anyhow::Result<PathBuf> {
    ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|d| d.is_dir())
        .context("set ANDROID_HOME to the Android SDK (with build-tools and a platform)")
}

/// The NDK's clang and LLVM tools for this host.
fn ndk_bin(ndk: &Path) -> anyhow::Result<PathBuf> {
    let prebuilt = ndk.join("toolchains/llvm/prebuilt");
    let host = newest(&prebuilt, |_| true).with_context(|| format!("no prebuilt toolchain in {}", prebuilt.display()))?;
    Ok(host.join("bin"))
}

/// The entry of `dir` with the highest version-sorted name that `keep` accepts.
fn newest(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut entries: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(&keep))
        .map(|e| (version_key(&e.file_name().to_string_lossy()), e.path()))
        .collect();
    entries.sort();
    entries.pop().map(|(_, p)| p)
}

/// `34.0.0` → [34, 0, 0], `android-34` → [34]: the numbers in a name, for sorting.
fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// A tool from the NDK's bin folder (`.cmd` wrappers on Windows hosts).
fn ndk_tool(bin: &Path, name: &str) -> PathBuf {
    let cmd = bin.join(format!("{name}.cmd"));
    if cfg!(windows) && cmd.is_file() { cmd } else { bin.join(name) }
}

/// Environment-variable form of a target triple (`aarch64-linux-android` → `AARCH64_LINUX_ANDROID`).
fn env_triple(triple: &str) -> String {
    triple.replace('-', "_").to_ascii_uppercase()
}

fn build_lib(abi: &Abi, bin: &Path, debug: bool, check: bool) -> anyhow::Result<()> {
    let clang = ndk_tool(bin, &format!("{}{MIN_SDK}-clang", abi.clang));
    if !clang.exists() {
        bail!("{} is missing: the NDK has no clang for {} at API {MIN_SDK}", clang.display(), abi.name);
    }
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root());
    if check {
        // The shell too: what only Android compiles (`cfg(target_os = "android")`) is checked here.
        cmd.args(["clippy", "-p", "pdfcraft-android", "-p", "pdfcraft-ui-egui", "--target", abi.triple, "--", "-D", "warnings"]);
    } else {
        cmd.args(["build", "-p", "pdfcraft-android", "--target", abi.triple]);
        if !debug {
            cmd.arg("--release");
        }
    }
    let lower = abi.triple.replace('-', "_");
    cmd.env(format!("CARGO_TARGET_{}_LINKER", env_triple(abi.triple)), &clang)
        .env(format!("CC_{lower}"), &clang)
        .env(format!("CXX_{lower}"), ndk_tool(bin, &format!("{}{MIN_SDK}-clang++", abi.clang)))
        .env(format!("AR_{lower}"), ndk_tool(bin, "llvm-ar"));
    let what = format!("cargo {} -p pdfcraft-android --target {}", if check { "clippy" } else { "build" }, abi.triple);
    eprintln!("$ {what}");
    let status = cmd.status().with_context(|| format!("{what}: failed to spawn"))?;
    if !status.success() {
        bail!("{what}: exited with {status}");
    }
    Ok(())
}

fn lib_path(abi: &Abi, debug: bool) -> PathBuf {
    target_dir().join(abi.triple).join(if debug { "debug" } else { "release" }).join(LIB)
}

/// The Android version code for `version` (`1.2.3` → 1002003): it must grow with every release.
fn version_code(version: &str) -> anyhow::Result<u32> {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<u32> = core.split('.').map(str::parse).collect::<Result<_, _>>().with_context(|| format!("version {version}"))?;
    let [major, minor, patch] = parts[..] else { bail!("version {version} is not X.Y.Z") };
    if minor > 999 || patch > 999 || major > 2000 {
        bail!("version {version} doesn't fit an Android version code");
    }
    Ok(major * 1_000_000 + minor * 1_000 + patch)
}

fn run_tool(cmd: &mut Command, what: &str) -> anyhow::Result<()> {
    eprintln!("$ {what}");
    let status = cmd.status().with_context(|| format!("{what}: failed to spawn"))?;
    if !status.success() {
        bail!("{what}: exited with {status}");
    }
    Ok(())
}

fn package(options: &Options, bin: &Path, tools: &Path, android_jar: &Path, version: &str) -> anyhow::Result<PathBuf> {
    let work = target_dir().join("android-apk");
    let _ = std::fs::remove_dir_all(&work);
    // Launcher icons: the app icon at the sizes it ships in, one per density bucket.
    let icons = root().join("assets/app-icon/hicolor");
    let res = work.join("res");
    for (density, size) in [("mdpi", 48), ("hdpi", 64), ("xhdpi", 128), ("xxhdpi", 128), ("xxxhdpi", 256)] {
        let dir = res.join(format!("mipmap-{density}"));
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(icons.join(format!("{size}x{size}/apps/ai.storyteller.pdfcraft.png")), dir.join("ic_launcher.png"))
            .with_context(|| format!("the {size} px app icon"))?;
    }
    // OCR models, when fetched (`cargo xtask models`): the app unpacks them on first start.
    let assets = work.join("assets");
    let models = root().join("assets/models");
    let mut have_models = 0;
    for name in ["text-detection.rten", "text-recognition.rten"] {
        let from = models.join(name);
        if from.is_file() {
            std::fs::create_dir_all(assets.join("models"))?;
            std::fs::copy(&from, assets.join("models").join(name))?;
            have_models += 1;
        }
    }
    if have_models < 2 {
        eprintln!("android: warning: no OCR models in assets/models (run `cargo xtask models`); Recognize Text won't work in this APK");
    }
    let compiled = work.join("res.zip");
    run_tool(Command::new(tools.join("aapt2")).arg("compile").arg("--dir").arg(&res).arg("-o").arg(&compiled), "aapt2 compile")?;
    let unsigned = work.join("unsigned.apk");
    let mut link = Command::new(tools.join("aapt2"));
    link.arg("link")
        .arg("-o")
        .arg(&unsigned)
        .arg("-I")
        .arg(android_jar)
        .arg("--manifest")
        .arg(root().join("packaging/android/AndroidManifest.xml"))
        .args(["--min-sdk-version", &MIN_SDK.to_string(), "--target-sdk-version", &TARGET_SDK.to_string()])
        .args(["--version-code", &version_code(version)?.to_string(), "--version-name", version]);
    if have_models > 0 {
        link.arg("-A").arg(&assets);
    }
    link.arg(&compiled);
    run_tool(&mut link, "aapt2 link")?;
    // The libraries, stripped, at lib/<abi>/ inside the APK (`aapt add` keeps the relative path).
    for abi in &options.abis {
        let dir = work.join("lib").join(abi.name);
        std::fs::create_dir_all(&dir)?;
        let built = lib_path(abi, options.debug);
        let to = dir.join(LIB);
        if options.debug {
            std::fs::copy(&built, &to).with_context(|| format!("{}", built.display()))?;
        } else {
            run_tool(Command::new(ndk_tool(bin, "llvm-strip")).arg("--strip-unneeded").arg("-o").arg(&to).arg(&built), "llvm-strip")?;
        }
        let inside = format!("lib/{}/{LIB}", abi.name);
        run_tool(Command::new(tools.join("aapt")).current_dir(&work).arg("add").arg(&unsigned).arg(&inside), &format!("aapt add {inside}"))?;
    }
    let aligned = work.join("aligned.apk");
    run_tool(Command::new(tools.join("zipalign")).args(["-p", "-f", "4"]).arg(&unsigned).arg(&aligned), "zipalign")?;
    let names: Vec<&str> = options.abis.iter().map(|a| a.name).collect();
    let out_dir = root().join("dist/android");
    std::fs::create_dir_all(&out_dir)?;
    let apk = out_dir.join(format!("pdfcraft-{version}-{}{}.apk", names.join("+"), if options.debug { "-debug" } else { "" }));
    let mut sign = Command::new(tools.join(if cfg!(windows) { "apksigner.bat" } else { "apksigner" }));
    sign.arg("sign");
    match std::env::var_os("PDFCRAFT_ANDROID_KEYSTORE") {
        Some(keystore) => {
            let alias = std::env::var("PDFCRAFT_ANDROID_KEY_ALIAS").unwrap_or_else(|_| "pdfcraft".into());
            // Passed by name, so the password never shows in the process list or this log.
            sign.arg("--ks").arg(keystore).args(["--ks-key-alias", &alias, "--ks-pass", "env:PDFCRAFT_ANDROID_KEYSTORE_PASS"]);
        }
        None => {
            let keystore = debug_keystore()?;
            eprintln!("android: signing with the debug keystore {} (for testing only)", keystore.display());
            sign.arg("--ks").arg(keystore).args(["--ks-key-alias", "androiddebugkey", "--ks-pass", "pass:android", "--key-pass", "pass:android"]);
        }
    }
    sign.arg("--out").arg(&apk).arg(&aligned);
    run_tool(&mut sign, "apksigner sign")?;
    Ok(apk)
}

/// The standard Android debug keystore, made with `keytool` when it doesn't exist yet.
fn debug_keystore() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).context("no home folder for the debug keystore")?;
    let dir = PathBuf::from(home).join(".android");
    let keystore = dir.join("debug.keystore");
    if keystore.is_file() {
        return Ok(keystore);
    }
    std::fs::create_dir_all(&dir)?;
    run_tool(
        Command::new("keytool")
            .args(["-genkeypair", "-keystore"])
            .arg(&keystore)
            .args(["-storepass", "android", "-alias", "androiddebugkey", "-keypass", "android", "-keyalg", "RSA", "-keysize", "2048"])
            .args(["-validity", "10000", "-dname", "CN=Android Debug,O=Android,C=US"]),
        "keytool -genkeypair (debug keystore)",
    )?;
    Ok(keystore)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_default_to_a_release_arm64_apk() {
        assert_eq!(parse(&[]).unwrap(), Options { abis: vec![ABIS[0]], debug: false, check: false });
        let o = parse(&args(&["--abi", "x86_64", "--abi", "x86_64", "--abi", "arm64-v8a", "--debug"])).unwrap();
        assert_eq!(o.abis.iter().map(|a| a.name).collect::<Vec<_>>(), ["x86_64", "arm64-v8a"]);
        assert!(o.debug);
        assert_eq!(parse(&args(&["--all-abis", "--check"])).unwrap().abis.len(), 4);
        assert!(parse(&args(&["--abi", "mips"])).is_err());
        assert!(parse(&args(&["--abi"])).is_err());
        assert!(parse(&args(&["--sign"])).is_err());
    }

    #[test]
    fn version_codes_grow_with_the_version() {
        assert_eq!(version_code("0.7.1").unwrap(), 7_001);
        assert_eq!(version_code("1.0.0-rc.1").unwrap(), 1_000_000);
        assert!(version_code("1.10.0").unwrap() > version_code("1.9.99").unwrap());
        for bad in ["1.2", "1.2.3.4", "x.y.z", "1.1000.0", ""] {
            assert!(version_code(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_newest_tools_are_found_by_version_not_by_name() {
        assert_eq!(version_key("34.0.0"), [34, 0, 0]);
        assert_eq!(version_key("android-34"), [34]);
        assert!(version_key("9.0.0") < version_key("10.0.0"));
        let dir = std::env::temp_dir().join(format!("xtask-android-newest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["9.0.0", "34.0.0", "10.0.1", "android-33", "android-35"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        assert_eq!(newest(&dir, |n| !n.starts_with("android-")), Some(dir.join("34.0.0")));
        assert_eq!(newest(&dir, |n| n.starts_with("android-")), Some(dir.join("android-35")));
        assert_eq!(newest(&dir.join("missing"), |_| true), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_manifest_names_the_library_and_has_no_internet_permission() {
        let manifest = std::fs::read_to_string(root().join("packaging/android/AndroidManifest.xml")).unwrap();
        let lib = LIB.trim_start_matches("lib").trim_end_matches(".so");
        assert!(manifest.contains(&format!("android:value=\"{lib}\"")), "android.app.lib_name must load {LIB}");
        assert!(manifest.contains("android:hasCode=\"false\""));
        assert!(manifest.contains("android.app.NativeActivity"));
        assert!(!manifest.contains("android.permission.INTERNET"), "PdfCraft sends nothing");
        // A document opened while PdfCraft runs reaches it through a second Activity (shell.rs).
        assert!(manifest.contains("android:launchMode=\"standard\""), "singleTask would drop documents opened while PdfCraft runs");
        assert!(manifest.contains("android.permission.REORDER_TASKS"));
        // Rotating or folding must not restart the activity: android_main can't run twice.
        for change in ["orientation", "screenSize", "smallestScreenSize", "screenLayout", "keyboardHidden", "uiMode", "density"] {
            assert!(manifest.contains(change), "configChanges lacks {change}");
        }
    }

    /// The unsafe-code exception (AGENTS.md §4) covers two items of the Android app and nothing
    /// else: every other crate inherits the workspace's `forbid`, which no `allow` can lift.
    #[test]
    fn unsafe_code_is_allowed_only_on_the_two_approved_android_items() {
        let pattern = ["allow", "(unsafe_code)"].concat();
        let mut found = Vec::new();
        let mut stack = vec![root().join("crates"), root().join("apps"), root().join("xtask")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let rel = path.strip_prefix(root()).unwrap().to_string_lossy().replace('\\', "/");
                    found.extend(std::iter::repeat_n(rel, text.matches(&pattern).count()));
                }
            }
        }
        found.sort();
        assert_eq!(found, ["apps/pdfcraft-android/src/jvm.rs", "apps/pdfcraft-android/src/lib.rs"]);
        let lints = std::fs::read_to_string(root().join("apps/pdfcraft-android/Cargo.toml")).unwrap();
        assert!(lints.contains("unsafe_code = \"deny\""), "the Android app still denies unsafe code by default");
    }
}
