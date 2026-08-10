fn main() {
    // Windows always builds dav1d from source: system-deps would otherwise
    // hard-fail on Windows (no pkg-config) instead of falling back to the
    // vendored meson+ninja build. CI sets this via GITHUB_ENV
    // (SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=always in ci.yml/release.yml) so
    // builds there work regardless; this only helps local Windows builds of
    // this crate and its consumers. The old committed .cargo/config.toml
    // used `[target.<triple>.env]` for this, which Cargo silently ignores
    // (target-scoped [env] is not a valid config table) — hence the env var
    // lives here and in the workflows instead.
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-env=SYSTEM_DEPS_DAV1D_BUILD_INTERNAL=always");
}
