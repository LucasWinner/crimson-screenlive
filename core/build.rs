//! Optionally embeds the app access key (repo-root `.app-key`, gitignored, or
//! `SCREENLIVE_APP_KEY` env) at compile time for local/CLI builds. Public
//! release builds leave this unset so users configure server + key at runtime.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".app-key");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed=SCREENLIVE_APP_KEY");
    if std::env::var_os("SCREENLIVE_APP_KEY").is_some() {
        return; // explicit build-time key wins
    }
    if let Ok(key) = std::fs::read_to_string(&path) {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            println!("cargo:rustc-env=SCREENLIVE_APP_KEY={trimmed}");
        }
    }
}
