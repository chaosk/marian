// What an image is called. `build.rs` includes this file.

/// A branch build is the commit. A tag build is the version number, without a
/// leading `v`. A detached commit that is not a tag stays the commit.
///
/// `build.rs` is the caller outside tests.
#[allow(dead_code)]
pub(crate) fn release_version(on_branch: bool, tag: Option<&str>, commit: &str) -> String {
    if on_branch {
        return commit.to_string();
    }
    let Some(tag) = tag.map(str::trim).filter(|tag| !tag.is_empty()) else {
        return commit.to_string();
    };
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_is_the_commit_and_a_tag_is_the_version() {
        assert_eq!(release_version(true, Some("v0.2.2"), "abc123"), "abc123");
        assert_eq!(release_version(true, None, "abc123"), "abc123");
        assert_eq!(release_version(false, Some("v0.2.2"), "abc123"), "0.2.2");
        assert_eq!(release_version(false, Some("0.2.2"), "abc123"), "0.2.2");
        assert_eq!(release_version(false, None, "abc123"), "abc123");
        assert_eq!(release_version(false, Some("  "), "abc123"), "abc123");
    }
}
