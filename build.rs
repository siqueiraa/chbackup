fn main() {
    // Capture git commit hash at build time.
    // In Docker, .git is excluded so fall back to VCS_REF build arg (passed via env).
    let git_sha = std::env::var("VCS_REF")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CHBACKUP_GIT_SHA={git_sha}");

    // Only re-run when source changes, not on every build.
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VCS_REF");
}
