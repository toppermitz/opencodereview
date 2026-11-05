### Changed

- Connect GitLab uses the Self-Hosted OAuth **device authorization grant** and stores the access token for `glab` as `OAUTH_TOKEN` (with `GLAB_IS_OAUTH2=true` so glab sends `Authorization: Bearer`), instead of running `glab auth login --device`. Set `OCR_GITLAB_CLIENT_ID` to a non-confidential OAuth Application ID with Device authorization enabled (Redirect URI can be `http://127.0.0.1/` for the form only).

### Fixed

- Repository onboarding lists namespaces from projects you can access, not only from `groups?membership=true`, so direct project membership under groups like `web/` or `bshop/` appears in Pick your repositories.
- `glab api --paginate` output that concatenates pages as successive JSON arrays is merged by parsing each page, so titles containing `][` stay intact and empty pages do not break the list.
- OAuth device-flow errors no longer echo raw token response bodies to the browser.
- Stored OAuth credentials are bound to the GitLab host and application ID; git uses a freshly resolved token (with refresh) instead of a stale cache; sign-out also clears a legacy `glab` session; refresh keeps the store across transient network failures.
