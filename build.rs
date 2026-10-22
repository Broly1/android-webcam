//! Makes the full release version (e.g. "1.0.4-5": Cargo's version plus the package revision
//! from [package.metadata.deb], which the Arch pkgrel matches) available as APP_VERSION, so the
//! About window shows the same version as the .deb and Arch packages.

use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let manifest = fs::read_to_string("Cargo.toml").unwrap();
    let revision = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("revision"))
        .and_then(|rest| rest.split('"').nth(1));
    let full = match revision {
        Some(rev) if !rev.is_empty() => format!("{}-{}", version, rev),
        _ => version,
    };
    println!("cargo:rustc-env=APP_VERSION={}", full);
}
