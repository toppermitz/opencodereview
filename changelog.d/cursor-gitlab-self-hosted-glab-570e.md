### Changed

- OpenCodeReview now talks to **GitLab Self-Hosted** through the authenticated `glab` CLI instead of GitHub/`gh`. Set `OCR_GITLAB_HOST` to your instance (bare `host` or `host:port`) before starting Compose. Reviews post as merge request notes and discussions.

### Removed

- GitHub/`gh` forge support, including `OCR_GH_HOST` and `GH_TOKEN`. Use `OCR_GITLAB_HOST` and optional `GITLAB_TOKEN` instead.
