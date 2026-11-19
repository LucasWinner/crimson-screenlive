//! Embeds the app access key (repo-root `.app-key`, gitignored) at compile
//! time as SCREENLIVE_APP_KEY. See scripts/gen-app-key.mjs.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".app-key");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed=SCREENLIVE_APP_KEY");
    if std::env::var_os("SCREENLIVE_APP_KEY").is_some() {
        return; // explicit build-time key wins (CI)
    }
    match std::fs::read_to_string(&path) {
        Ok(key) if !key.trim().is_empty() => println!("cargo:rustc-env=SCREENLIVE_APP_KEY={}", key.trim()),
        _ => println!(
            "cargo:warning=no .app-key found: this build can't use a server that requires the app key \
             (run `node scripts/gen-app-key.mjs`)"
        ),
    }
}
