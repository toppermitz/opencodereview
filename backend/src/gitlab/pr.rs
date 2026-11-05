use super::*;

/// Valid inline-comment anchors for an MR, parsed from its unified diff: per file
/// path, the line numbers commentable on each side — `.0` = RIGHT (new file),
/// `.1` = LEFT (old file).
pub fn diff_anchors(full_name: &str, number: i64) -> HashMap<String, (HashSet<i64>, HashSet<i64>)> {
    let mut map: HashMap<String, (HashSet<i64>, HashSet<i64>)> = HashMap::new();
    let diff = match run_raw("glab", &["mr", "diff", &number.to_string(), "-R", full_name]) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return map,
    };
    let mut path: Option<String> = None;
    let mut old_path: Option<String> = None;
    let (mut old_ln, mut new_ln) = (0i64, 0i64);
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("--- a/") {
            old_path = Some(rest.trim().to_string());
            path = None;
        } else if line.starts_with("--- ") {
            old_path = None;
            path = None;
        } else if let Some(rest) = line.strip_prefix("+++ b/") {
            let name = rest.trim().to_string();
            map.entry(name.clone()).or_default();
            path = Some(name);
        } else if line.starts_with("+++ /dev/null") {
            if let Some(name) = old_path.take() {
                map.entry(name.clone()).or_default();
                path = Some(name);
            }
        } else if line.starts_with("+++") {
            path = None;
        } else if line.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_starts(line) {
                old_ln = old_start;
                new_ln = new_start;
            }
        } else if let Some(entry) = path.as_ref().and_then(|p| map.get_mut(p)) {
            match line.as_bytes().first() {
                Some(b'+') => {
                    entry.0.insert(new_ln);
                    new_ln += 1;
                }
                Some(b'-') => {
                    entry.1.insert(old_ln);
                    old_ln += 1;
                }
                Some(b' ') => {
                    entry.0.insert(new_ln);
                    entry.1.insert(old_ln);
                    new_ln += 1;
                    old_ln += 1;
                }
                _ => {}
            }
        }
    }
    map
}

pub(crate) fn parse_hunk_starts(line: &str) -> Option<(i64, i64)> {
    let (mut old_start, mut new_start) = (None, None);
    for token in line.split("@@").nth(1)?.split_whitespace() {
        if let Some(rest) = token.strip_prefix('-') {
            old_start = rest.split(',').next().and_then(|n| n.parse::<i64>().ok());
        } else if let Some(rest) = token.strip_prefix('+') {
            new_start = rest.split(',').next().and_then(|n| n.parse::<i64>().ok());
        }
    }
    Some((old_start?, new_start?))
}

fn classify_glab_error(err: &str) -> &'static str {
    let bounded: String = err.chars().take(200).collect();
    let lower = bounded.to_ascii_lowercase();
    if lower.contains("auth")
        || lower.contains("login")
        || lower.contains("token")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("credentials")
    {
        "authentication failure: verify GitLab CLI authentication and repository access"
    } else if lower.contains("could not resolve host")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("network")
        || lower.contains("timed out")
    {
        "transport failure: check network connectivity and retry"
    } else {
        "command error: GitLab CLI could not load the MR description"
    }
}

pub(crate) fn parse_pr_description_payload(
    full_name: &str,
    number: i64,
    bytes: &[u8],
) -> Result<(String, String), String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Malformed MR description JSON for {full_name}!{number}: {error}"))?;
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("Malformed MR description JSON for {full_name}!{number}: missing or invalid 'title' string")
        })?
        .to_string();
    let body = value
        .get("description")
        .or_else(|| value.get("body"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("Malformed MR description JSON for {full_name}!{number}: missing or invalid 'description' string")
        })?
        .to_string();
    Ok((title, body))
}

pub fn pr_description(full_name: &str, number: i64) -> Result<(String, String), String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let bytes = run_raw("glab", &["api", &route]).map_err(|error| {
        format!(
            "Failed to load MR description for {full_name}!{number}: {}",
            classify_glab_error(&error)
        )
    })?;
    parse_pr_description_payload(full_name, number, &bytes)
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn pr_file_paths(full_name: &str, number: i64) -> Result<Vec<String>, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}/changes");
    let bytes = transport::glab_capture(&["api", &route])?;
    parse_pr_file_pages(&bytes)
}

pub(crate) fn parse_pr_file_pages(bytes: &[u8]) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let changes = value
        .get("changes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "GitLab returned merge request changes without a list".to_string())?;
    changes
        .iter()
        .map(|file| {
            file.get("new_path")
                .or_else(|| file.get("old_path"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
                .ok_or_else(|| "GitLab returned a changed file without a name".to_string())
        })
        .collect()
}

#[cfg(test)]
mod hunk_regression_tests {
    #[test]
    fn hunk_positions_ignore_function_context() {
        for context in [
            "fn normalize(team: &str) -> Vec<String> {",
            "fn sum() + extra - context",
            "",
        ] {
            assert_eq!(
                super::parse_hunk_starts(&format!("@@ -440,3 +306,18 @@ {context}")),
                Some((440, 306))
            );
        }
        assert_eq!(super::parse_hunk_starts("@@ -1 +2 @@"), Some((1, 2)));
        assert_eq!(super::parse_hunk_starts("not a hunk"), None);
    }

    #[test]
    fn glab_error_classification_identifies_auth_transport_and_command() {
        let auth_err = super::classify_glab_error("HTTP 403 Forbidden");
        assert!(auth_err.starts_with("authentication failure:"), "{auth_err}");

        let network_err = super::classify_glab_error(
            "fatal: unable to access 'https://gitlab.example.com/': Could not resolve host: gitlab.example.com",
        );
        assert!(network_err.starts_with("transport failure:"), "{network_err}");

        let cmd_err = super::classify_glab_error("merge request not found");
        assert!(cmd_err.starts_with("command error:"), "{cmd_err}");
    }

    #[test]
    fn glab_error_messages_never_include_command_output() {
        let secret = "fixture-private-value";
        for error in [
            format!("authentication failed: Authorization: Bearer {secret}"),
            format!("connection refused for https://user:{secret}@gitlab.example.com/acme/api"),
            format!("unexpected response: ?access_key={secret}"),
            format!("{} password={secret}", "é".repeat(250)),
        ] {
            let classified = super::classify_glab_error(&error);
            assert!(!classified.contains(secret));
            assert!(!classified.contains("user:"));
            assert!(!classified.contains("access_key"));
            assert!(!classified.contains("password="));
        }
    }

    #[test]
    fn pr_description_payload_requires_valid_strings_and_permits_empty_body() {
        let valid = br#"{"title":"Add feature","description":"Description text"}"#;
        assert_eq!(
            super::parse_pr_description_payload("owner/repo", 1, valid).unwrap(),
            ("Add feature".into(), "Description text".into())
        );

        let empty_body = br#"{"title":"Quick fix","description":""}"#;
        assert_eq!(
            super::parse_pr_description_payload("owner/repo", 2, empty_body).unwrap(),
            ("Quick fix".into(), "".into())
        );

        let missing_body = br#"{"title":"No body"}"#;
        let err = super::parse_pr_description_payload("owner/repo", 3, missing_body).unwrap_err();
        assert!(err.contains("missing or invalid 'description' string"), "{err}");

        let missing_title = br#"{"description":"Some body"}"#;
        let err = super::parse_pr_description_payload("owner/repo", 4, missing_title).unwrap_err();
        assert!(err.contains("missing or invalid 'title' string"), "{err}");
    }
}
