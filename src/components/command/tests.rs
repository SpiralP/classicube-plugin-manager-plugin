use super::*;

#[test]
fn valid_owner_repo() {
    assert_eq!(
        parse_owner_repo("octocat/hello-world"),
        Some(("octocat".into(), "hello-world".into()))
    );
}

#[test]
fn allows_dots_hyphens_underscores() {
    assert_eq!(
        parse_owner_repo("hyphen-name/under_score.dot"),
        Some(("hyphen-name".into(), "under_score.dot".into()))
    );
}

#[test]
fn empty_string_rejected() {
    assert_eq!(parse_owner_repo(""), None);
}

#[test]
fn missing_repo_rejected() {
    assert_eq!(parse_owner_repo("owner/"), None);
}

#[test]
fn missing_owner_rejected() {
    assert_eq!(parse_owner_repo("/repo"), None);
}

#[test]
fn no_slash_rejected() {
    assert_eq!(parse_owner_repo("ownerrepo"), None);
}

#[test]
fn whitespace_in_owner_rejected() {
    assert_eq!(parse_owner_repo("a b/repo"), None);
    assert_eq!(parse_owner_repo("owner /repo"), None);
}

#[test]
fn whitespace_in_repo_rejected() {
    assert_eq!(parse_owner_repo("owner/ repo"), None);
    assert_eq!(parse_owner_repo("owner/re po"), None);
}

#[test]
fn extra_slash_rejected() {
    // split_once takes the first '/', so "a/b/c" → owner="a", repo="b/c".
    // The repo-side slash check rejects it.
    assert_eq!(parse_owner_repo("a/b/c"), None);
}
