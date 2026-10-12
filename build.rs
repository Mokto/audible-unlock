use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/Login.swift");
    println!("cargo:rerun-if-changed=native/OnePassword.swift");
    let helper = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("audible-login");
    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-swift-version",
            "5",
            "-Onone",
            "native/Login.swift",
            "native/OnePassword.swift",
            "-o",
        ])
        .arg(&helper)
        .env("MACOSX_DEPLOYMENT_TARGET", "12.0")
        .status()
        .expect("The macOS Command Line Tools are required to compile the WebKit login window");
    assert!(
        status.success(),
        "Unable to compile the native WebKit login helper"
    );
    println!("cargo:rustc-env=AUDIBLE_LOGIN_HELPER={}", helper.display());
}
