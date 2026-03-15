use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let counter_path = manifest_dir.join(".build-number");
    let current = fs::read_to_string(&counter_path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = current.saturating_add(1);
    fs::write(&counter_path, format!("{next}\n")).expect("write build number");

    println!("cargo:rerun-if-changed={}", counter_path.display());
    println!("cargo:rustc-env=TELECODEX_BUILD_NUMBER={next}");
    println!(
        "cargo:rustc-env=TELECODEX_APP_VERSION={}",
        env::var("CARGO_PKG_VERSION").expect("pkg version")
    );
}
