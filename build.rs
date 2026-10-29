use std::{env, fs, path::PathBuf, process::Command};

fn windows_resource_compiler() -> PathBuf {
    if let Some(custom_rc) = env::var_os("RC") {
        return custom_rc.into();
    }

    // rc.exe is part of the Windows SDK installed alongside MSVC. It is often
    // absent from PATH when Cargo is launched from a normal terminal.
    let sdk_root = env::var_os("WindowsSdkDir")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("ProgramFiles(x86)").unwrap_or_default())
                .join("Windows Kits")
                .join("10")
        });
    let bin_dir = sdk_root.join("bin");
    let mut compilers: Vec<_> = fs::read_dir(bin_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|item| item.path().join("x64").join("rc.exe"))
        .filter(|file| file.is_file())
        .collect();
    compilers.sort();
    compilers.pop().unwrap_or_else(|| PathBuf::from("rc.exe"))
}

fn main() {
    println!("cargo:rerun-if-env-changed=YTD_EMBEDDED_RUNTIME_PAYLOAD");
    println!("cargo:rerun-if-env-changed=YTD_RUNTIME_PAYLOAD_SHA256");
    println!("cargo:rerun-if-env-changed=YTD_REQUIRE_EMBEDDED_RUNTIME");

    // A normal developer build remains possible without the large runtime
    // bundle. The standalone packaging path opts into fail-closed behaviour.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let embedded_payload = env::var_os("YTD_EMBEDDED_RUNTIME_PAYLOAD").map(PathBuf::from);
    let require_payload = env::var("YTD_REQUIRE_EMBEDDED_RUNTIME").as_deref() == Ok("1");
    let payload_output = out_dir.join("runtime-payload.bin");
    let payload_hash = env::var("YTD_RUNTIME_PAYLOAD_SHA256").unwrap_or_default();
    if let Some(source) = embedded_payload.filter(|path| path.is_file()) {
        println!("cargo:rerun-if-changed={}", source.display());
        let metadata = fs::metadata(&source).expect("runtime payload metadata must be readable");
        assert!(metadata.len() > 12, "runtime payload is empty or truncated");
        if require_payload {
            assert!(
                payload_hash.len() == 64
                    && payload_hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "standalone packaging requires the payload SHA-256"
            );
        }
        fs::copy(&source, &payload_output).expect("runtime payload must be copied to OUT_DIR");
        println!("cargo:rustc-env=YTD_RUNTIME_PAYLOAD_SHA256={payload_hash}");
    } else {
        assert!(
            !require_payload,
            "standalone packaging requested but YTD_EMBEDDED_RUNTIME_PAYLOAD is missing"
        );
        fs::write(&payload_output, []).expect("empty development payload must be created");
        println!("cargo:rustc-env=YTD_RUNTIME_PAYLOAD_SHA256=");
    }

    slint_build::compile("ui/main.slint").expect("Slint UI must compile");

    println!("cargo:rerun-if-changed=assets/windows-icon.rc");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    println!("cargo:rerun-if-env-changed=RC");

    // The Slint Window.icon property controls the live taskbar/title-bar
    // image. The native PE icon resource also gives File Explorer, shortcuts,
    // and the EXE itself the same branding before the window is launched.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("app-icon.res");
        let status = Command::new(windows_resource_compiler())
            .current_dir(manifest_dir.join("assets"))
            .arg("/nologo")
            .arg("/fo")
            .arg(&output)
            .arg("windows-icon.rc")
            .status()
            .expect("Windows SDK resource compiler required for EXE icon");
        assert!(status.success(), "Windows resource icon compilation failed");
        println!("cargo:rustc-link-arg-bins={}", output.display());
    }
}
