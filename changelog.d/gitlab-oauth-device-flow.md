### Changed

- Connect GitLab uses the Self-Hosted OAuth **device authorization grant** and stores the access token for `glab` as `OAUTH_TOKEN` (with `GLAB_IS_OAUTH2=true` so glab sends `Authorization: Bearer`), instead of running `glab auth login --device`. Set `OCR_GITLAB_CLIENT_ID` to a non-confidential OAuth Application ID with Device authorization enabled (Redirect URI can be `http://127.0.0.1/` for the form only).

### Fixed

- Repository onboarding lists namespaces from projects you can access, not only from `groups?membership=true`, so direct project membership under groups like `web/` or `bshop/` appears in Pick your repositories.
- `glab api --paginate` output that concatenates pages as `][` is merged before parsing, so large project lists no longer fail with a JSON error.
