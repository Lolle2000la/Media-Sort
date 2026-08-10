fn main() {
    #[cfg(all(target_os = "windows", feature = "vendor-windows"))]
    setup_mpv_windows();
}

/// Default mpv-winbuild release tag. Override via the `MPV_WINBUILD_TAG`
/// environment variable (release/CI workflows pin this; the
/// `bump-mpv-winbuild` workflow updates the default monthly).
#[allow(dead_code)] // only read by the Windows vendoring path
const DEFAULT_MPV_WINBUILD_TAG: &str = "2026-08-08-dd5d17d328";

/// Fails the build script with a `cargo:warning` naming `step` and `detail`.
///
/// Build scripts cannot return errors, so a failed vendoring step aborts the
/// build the way the old workflow shell scripts did: abort-loud instead of
/// silently linking against stale or absent binaries.
#[cfg(all(target_os = "windows", feature = "vendor-windows"))]
fn fail(step: &str, detail: &str) -> ! {
    println!("cargo:warning={step} failed: {detail}");
    std::process::exit(1);
}

/// Runs `cmd` and fails the build loudly when it does not exit successfully.
#[cfg(all(target_os = "windows", feature = "vendor-windows"))]
fn run_or_fail(cmd: &mut std::process::Command, step: &str) {
    let output = match cmd.output() {
        Ok(out) => out,
        Err(e) => fail(step, &e.to_string()),
    };
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        fail(
            step,
            &format!(
                "exit status {:?}\nstdout: {stdout}\nstderr: {stderr}",
                output.status.code()
            ),
        );
    }
}

#[cfg(all(target_os = "windows", feature = "vendor-windows"))]
fn setup_mpv_windows() {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    // A tag bump must trigger a re-download even when the cargo target/
    // cache (7-day retention on CI) still holds the old binaries.
    println!("cargo:rerun-if-env-changed=MPV_WINBUILD_TAG");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    let mpv_arch = match target_arch.as_str() {
        "x86_64" => "x86_64",
        "x86" => "i686",
        "aarch64" => "aarch64",
        other => other,
    };
    let tag = env::var("MPV_WINBUILD_TAG").unwrap_or_else(|_| DEFAULT_MPV_WINBUILD_TAG.to_string());

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_dir = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&manifest_dir);
    let vendor_dir = workspace_dir
        .join("target")
        .join(format!("mpv-winbuild-{mpv_arch}"));

    let lib_a = vendor_dir.join("libmpv.dll.a");
    let dll = vendor_dir.join("libmpv-2.dll");
    let ffmpeg_exe = vendor_dir.join("ffmpeg.exe");

    // Records which tag the staged binaries came from: with a matching stamp
    // (and present binaries) nothing is re-downloaded, and a mismatched stamp
    // forces a fresh download even though the old files still exist.
    let stamp = vendor_dir.join("tag.txt");
    let stamp_matches = fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == tag);

    if !lib_a.exists() || !dll.exists() || !ffmpeg_exe.exists() || !stamp_matches {
        fs::create_dir_all(&vendor_dir)
            .unwrap_or_else(|e| fail("creating the vendor dir", &e.to_string()));

        let release_api_url =
            format!("https://api.github.com/repos/Lolle2000la/mpv-winbuild/releases/tags/{tag}");

        // `-f` turns HTTP errors (404 for a missing tag, rate limits) into a
        // failing curl exit. GH_TOKEN is optional but doubles the
        // unauthenticated hourly rate limit on CI.
        let mut curl = Command::new("curl.exe");
        curl.args(["-fL", "-H", "User-Agent: Rust-Build-Script"]);
        if let Ok(token) = env::var("GH_TOKEN") {
            let auth_header = format!("Authorization: Bearer {token}");
            curl.args(["-H", auth_header.as_str()]);
        }
        curl.arg(&release_api_url);

        let body = match curl.output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => fail(
                "querying the mpv-winbuild release API",
                &format!("curl exited with {:?}", out.status.code()),
            ),
            Err(e) => fail("querying the mpv-winbuild release API", &e.to_string()),
        };

        let release: serde_json::Value = serde_json::from_str(&body).unwrap_or_else(|e| {
            fail(
                "parsing the mpv-winbuild release API response",
                &e.to_string(),
            )
        });

        let mut mpv_url = None;
        let mut ffmpeg_url = None;
        if let Some(assets) = release.get("assets").and_then(|a| a.as_array()) {
            for asset in assets {
                let Some(name) = asset.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let Some(url) = asset.get("browser_download_url").and_then(|u| u.as_str()) else {
                    continue;
                };
                if name.starts_with(&format!("mpv-dev-{mpv_arch}"))
                    && name.ends_with(".7z")
                    && !name.contains("-lgpl")
                    && !name.contains("-v3")
                {
                    mpv_url = Some(url.to_string());
                } else if name.starts_with(&format!("ffmpeg-{mpv_arch}"))
                    && name.ends_with(".7z")
                    && !name.contains("-lgpl")
                    && !name.contains("-v3")
                {
                    ffmpeg_url = Some(url.to_string());
                }
            }
        }

        let (Some(mpv_dl), Some(ffmpeg_dl)) = (mpv_url, ffmpeg_url) else {
            fail(
                "resolving mpv-winbuild release assets",
                &format!("no mpv-dev-{mpv_arch}/ffmpeg-{mpv_arch} archives found for tag {tag}"),
            );
        };

        let mpv_archive = vendor_dir.join("mpv-dev.7z");
        let ffmpeg_archive = vendor_dir.join("ffmpeg.7z");

        run_or_fail(
            Command::new("curl.exe").args([
                "-fL",
                "-o",
                mpv_archive.to_str().unwrap(),
                mpv_dl.as_str(),
            ]),
            "downloading the mpv-dev archive",
        );
        run_or_fail(
            Command::new("curl.exe").args([
                "-fL",
                "-o",
                ffmpeg_archive.to_str().unwrap(),
                ffmpeg_dl.as_str(),
            ]),
            "downloading the ffmpeg archive",
        );

        // 7-Zip ships with Windows; bsdtar is the fallback. Extract only the
        // members this build needs (wildcards match at any archive depth).
        let has_7z = Command::new("7z.exe")
            .arg("--help")
            .status()
            .is_ok_and(|s| s.success());

        if has_7z {
            let out_flag = format!("-o{}", vendor_dir.display());
            run_or_fail(
                Command::new("7z.exe").args([
                    "e",
                    mpv_archive.to_str().unwrap(),
                    "*libmpv-2.dll",
                    "*libmpv.dll.a",
                    out_flag.as_str(),
                    "-y",
                ]),
                "extracting the mpv-dev archive",
            );
            run_or_fail(
                Command::new("7z.exe").args([
                    "e",
                    ffmpeg_archive.to_str().unwrap(),
                    "*ffmpeg.exe",
                    out_flag.as_str(),
                    "-y",
                ]),
                "extracting the ffmpeg archive",
            );
        } else {
            run_or_fail(
                Command::new("tar.exe").args([
                    "-xf",
                    mpv_archive.to_str().unwrap(),
                    "-C",
                    vendor_dir.to_str().unwrap(),
                ]),
                "extracting the mpv-dev archive",
            );
            run_or_fail(
                Command::new("tar.exe").args([
                    "-xf",
                    ffmpeg_archive.to_str().unwrap(),
                    "-C",
                    vendor_dir.to_str().unwrap(),
                ]),
                "extracting the ffmpeg archive",
            );
        }

        for (label, path) in [
            ("libmpv.dll.a", &lib_a),
            ("libmpv-2.dll", &dll),
            ("ffmpeg.exe", &ffmpeg_exe),
        ] {
            if !path.exists() {
                fail(
                    "extracting the vendored archives",
                    &format!("{label} is missing after extraction"),
                );
            }
        }

        // The archives are only needed for extraction; drop them so the
        // cached target/ dir stays small.
        let _ = fs::remove_file(&mpv_archive);
        let _ = fs::remove_file(&ffmpeg_archive);

        fs::write(&stamp, format!("{tag}\n"))
            .unwrap_or_else(|e| fail("writing the mpv-winbuild tag stamp", &e.to_string()));
    }

    if vendor_dir.exists() {
        // The mpv-dev archive's import library is a mingw-style
        // `libmpv.dll.a`; libmpv-sys links `-lmpv`, which MSVC resolves as
        // `mpv.lib`. Mingw import libraries are COFF, so the rename works.
        if lib_a.exists() {
            let mpv_lib = vendor_dir.join("mpv.lib");
            fs::copy(&lib_a, &mpv_lib)
                .unwrap_or_else(|e| fail("staging the mpv import library", &e.to_string()));
        }

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        if let Some(profile_dir) = out_dir.ancestors().nth(3) {
            // `cargo test` binaries run from target/<triple>/<profile>/deps/
            // and the Windows loader resolves DLLs from the exe's own
            // directory first, so stage into deps/ as well as the profile dir.
            let deps_dir = profile_dir.join("deps");
            fs::create_dir_all(&deps_dir)
                .unwrap_or_else(|e| fail("creating the deps staging dir", &e.to_string()));
            for dest in [profile_dir, deps_dir.as_path()] {
                if dll.exists() {
                    fs::copy(&dll, dest.join("libmpv-2.dll"))
                        .unwrap_or_else(|e| fail("staging libmpv-2.dll", &e.to_string()));
                }
                if ffmpeg_exe.exists() {
                    fs::copy(&ffmpeg_exe, dest.join("ffmpeg.exe"))
                        .unwrap_or_else(|e| fail("staging ffmpeg.exe", &e.to_string()));
                }
            }
        }

        println!("cargo:rustc-link-search=native={}", vendor_dir.display());
    }

    println!("cargo:rerun-if-changed=build.rs");
}
