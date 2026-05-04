use super::*;

#[test]
fn debug_does_not_leak() {
    let s = Secret::new("github_pat_supersecret".into());
    let dbg = format!("{s:?}");
    assert!(!dbg.contains("supersecret"), "debug output leaked: {dbg}");
    assert_eq!(dbg, "Secret(<redacted>)");
}

#[test]
fn expose_returns_inner() {
    let s = Secret::new("abc".into());
    assert_eq!(s.expose(), "abc");
}

#[test]
fn serde_round_trip() {
    let s = Secret::new("ghp_xyz".into());
    let toml_value = toml::Value::try_from(&s).unwrap();
    assert_eq!(toml_value.as_str(), Some("ghp_xyz"));
    let back: Secret = toml_value.try_into().unwrap();
    assert_eq!(back.expose(), "ghp_xyz");
}
