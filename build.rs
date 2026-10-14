include!("src/release_version.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=MARIAN_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    let version = std::env::var("MARIAN_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(version_from_git);
    println!("cargo:rustc-env=MARIAN_VERSION={version}");
}

fn version_from_git() -> String {
    let Some(commit) = git(&["rev-parse", "HEAD"]) else {
        return "unknown".into();
    };
    let on_branch = git(&["symbolic-ref", "-q", "HEAD"]).is_some();
    let tag = if on_branch {
        None
    } else {
        git(&["describe", "--exact-match", "--tags", "HEAD"])
    };
    release_version(on_branch, tag.as_deref(), &commit)
}

fn git(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}
