use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/Login.swift");
    println!("cargo:rerun-if-changed=native/OnePassword.swift");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let helper = out.join("earshelf-login");
    let mut slices = vec![];
    for arch in ["arm64", "x86_64"] {
        let slice = out.join(format!("earshelf-login-{arch}"));
        let status = Command::new("xcrun")
            .args(["swiftc", "-swift-version", "5", "-Onone", "-target"])
            .arg(format!("{arch}-apple-macos12.0"))
            .args(["native/Login.swift", "native/OnePassword.swift", "-o"])
            .arg(&slice)
            .status()
            .expect("The macOS Command Line Tools are required to compile the WebKit login window");
        assert!(
            status.success(),
            "Unable to compile the native WebKit login helper"
        );
        slices.push(slice);
    }
    let status = Command::new("lipo")
        .arg("-create")
        .args(&slices)
        .arg("-output")
        .arg(&helper)
        .status()
        .expect("lipo is required to build the universal login helper");
    assert!(
        status.success(),
        "Unable to build the universal login helper"
    );
    println!("cargo:rustc-env=EARSHELF_LOGIN_HELPER={}", helper.display());
}
