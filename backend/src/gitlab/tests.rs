use super::*;

#[test]
fn cancelled_evidence_capture_never_starts_a_gitlab_request() {
    let cancel = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(
        glab_capture_cancellable(&["api", "user"], &cancel).unwrap_err(),
        "cancelled"
    );
}

#[test]
fn parses_live_pr_state_without_unsupported_fields() {
    assert_eq!(LIVE_PR_JSON_FIELDS, "state,sha,diff_refs,target_branch,merged_at");
    for (raw_state, expected) in [("opened", "OPEN"), ("closed", "CLOSED"), ("merged", "MERGED")] {
        let raw = format!(
            r#"{{"state":"{raw_state}","sha":"abc123","target_branch":"parent","diff_refs":{{"base_sha":"base123","head_sha":"abc123"}},"merged_at":"2026-09-27T00:47:42Z"}}"#
        );
        let parsed = parse_live_pr_state(raw.as_bytes()).expect("parse live MR state");
        assert_eq!(parsed.state, expected);
        assert_eq!(parsed.head_sha, "abc123");
        assert_eq!(parsed.base_ref, "parent");
        assert_eq!(parsed.base_sha, "base123");
        assert_eq!(parsed.merged_at, Some(1790470062000));
    }
    let raw_unmerged = r#"{"state":"opened","sha":"abc123","target_branch":"parent","diff_refs":{"base_sha":"base123","head_sha":"abc123"},"merged_at":null}"#;
    let parsed = parse_live_pr_state(raw_unmerged.as_bytes()).expect("parse unmerged");
    assert_eq!(parsed.merged_at, None);
}

#[test]
fn accepts_plain_hosts_and_ports() {
    for host in [
        "gitlab.example.com",
        "ghe.example.com",
        "ghe.internal",
        "localhost",
        "git.example.co.uk",
        "gitlab.example.com:8443",
    ] {
        assert!(is_valid_gitlab_host(host), "should accept {host}");
    }
}

#[test]
fn rejects_schemes_paths_creds_and_whitespace() {
    for host in [
        "",
        " ",
        "https://gitlab.example.com",
        "gitlab.example.com/group/project",
        "gitlab.example.com/",
        "user@gitlab.example.com",
        "gitlab.example.com ",
        "evil.com\ngitlab.example.com",
        "gitlab.example.com:",
        "gitlab.example.com:port",
        "-gitlab.example.com",
        "gitlab-.example.com",
        "git_lab.example.com",
    ] {
        assert!(!is_valid_gitlab_host(host), "should reject {host:?}");
    }
}

#[test]
fn sign_out_treats_an_absent_session_as_done_and_reports_real_failures() {
    assert!(auth::sign_out_result(true, "").is_ok());
    assert!(auth::sign_out_result(false, "not logged in to any hosts\n").is_ok());
    assert_eq!(
        auth::sign_out_result(false, "some noise\nfailed to write config: read-only file system\n\n").unwrap_err(),
        "Could not sign out of GitLab: failed to write config: read-only file system"
    );
}

#[test]
fn forgetting_the_identity_clears_cached_login_and_teams() {
    *GL_LOGIN.lock().unwrap() = Some("octocat".into());
    *GL_TEAM_SLUGS.lock().unwrap() = Some(HashSet::from(["core".to_string()]));
    auth::forget_identity();
    assert!(GL_LOGIN.lock().unwrap().is_none());
    assert!(GL_TEAM_SLUGS.lock().unwrap().is_none());
}

#[test]
fn project_api_id_encodes_nested_paths() {
    assert_eq!(auth::project_api_id("acme/api"), "acme%2Fapi");
    assert_eq!(auth::project_api_id("acme/team/api"), "acme%2Fteam%2Fapi");
}
