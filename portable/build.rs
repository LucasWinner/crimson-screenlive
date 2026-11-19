//! Embeds the portable app ZIP (made by app/scripts/make-portable.mjs, passed in
//! SCREENLIVE_PORTABLE_PAYLOAD) and the app icon. Without a payload the
//! launcher still builds (so `cargo build --workspace` works) but refuses to run.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SCREENLIVE_PORTABLE_PAYLOAD");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("payload.zip");
    let bytes = match std::env::var("SCREENLIVE_PORTABLE_PAYLOAD") {
        Ok(path) => {
            println!("cargo:rerun-if-changed={path}");
            std::fs::read(&path).unwrap_or_else(|e| panic!("can't read payload {path}: {e}"))
        }
        Err(_) => Vec::new(),
    };
    std::fs::write(&out, &bytes).unwrap();
    // Cache key: a new payload unpacks into a new folder.
    println!("cargo:rustc-env=PAYLOAD_CRC={:08x}", crc32fast::hash(&bytes));

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../app/src-tauri/icons/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon.to_str().unwrap())
            .set("FileDescription", "Crimson ScreenLive")
            .set("ProductName", "Crimson ScreenLive");
        res.compile().expect("embedding icon");
    }
}
