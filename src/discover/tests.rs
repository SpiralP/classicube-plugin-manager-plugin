use std::collections::BTreeSet;

use super::*;

#[test]
fn parse_bundled_list_succeeds() {
    let es = entries();
    assert!(!es.is_empty(), "bundled curated list should have entries");
    for e in es {
        assert!(!e.owner.is_empty(), "entry owner empty: {e:?}");
        assert!(!e.repo.is_empty(), "entry repo empty: {e:?}");
        assert!(!e.description.is_empty(), "entry description empty: {e:?}");
    }
}

#[test]
fn bundled_shorthands_unique_case_insensitive() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for e in entries() {
        if let Some(s) = &e.shorthand {
            assert!(
                !s.is_empty(),
                "shorthand for {}/{} is empty",
                e.owner,
                e.repo
            );
            let key = s.to_ascii_lowercase();
            assert!(seen.insert(key), "duplicate shorthand {s:?}");
        }
    }
}

#[test]
fn bundled_owner_repo_unique() {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for e in entries() {
        let key = (e.owner.to_ascii_lowercase(), e.repo.to_ascii_lowercase());
        assert!(
            seen.insert(key),
            "duplicate owner/repo: {}/{}",
            e.owner,
            e.repo
        );
    }
}

#[test]
fn lookup_shorthand_case_insensitive() {
    let entry = lookup_shorthand("CEF").expect("cef should resolve");
    assert_eq!(entry.shorthand.as_deref(), Some("cef"));
    assert_eq!(entry.owner, "SpiralP");
    assert_eq!(entry.repo, "classicube-cef-loader-plugin");
}

#[test]
fn lookup_shorthand_miss_returns_none() {
    assert!(lookup_shorthand("zzz-no-such-shorthand").is_none());
}

#[test]
fn iter_filtered_no_term_returns_all() {
    let total = entries().len();
    assert_eq!(iter_filtered(None).count(), total);
}

#[test]
fn iter_filtered_matches_repo_substring() {
    let hits: Vec<_> = iter_filtered(Some("cef")).collect();
    assert!(hits.iter().any(|e| e.repo.contains("cef")));
}

#[test]
fn iter_filtered_matches_shorthand() {
    let hits: Vec<_> = iter_filtered(Some("deno")).collect();
    assert!(hits.iter().any(|e| e.shorthand.as_deref() == Some("deno")));
}

#[test]
fn iter_filtered_matches_description_case_insensitively() {
    let hits: Vec<_> = iter_filtered(Some("CHROMIUM")).collect();
    assert!(
        hits.iter()
            .any(|e| e.description.to_ascii_lowercase().contains("chromium")),
        "expected at least one entry whose description mentions chromium"
    );
}

#[test]
fn iter_filtered_no_match_returns_empty() {
    assert_eq!(iter_filtered(Some("zzz-no-such-token")).count(), 0);
}
