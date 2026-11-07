### Added

- Clean reviews wait for a human decision by default. Turn on **Auto-aprovar MRs limpos** in Settings (with a max risk score) to post GitLab `/approve` automatically when there are no findings.
- Approving or requesting changes on GitLab posts the `/approve` or `/request_changes` quick action on the review note.
- A later review resolves OpenCodeReview discussions that the new result no longer keeps.

### Fixed

- Clean reviews no longer skip the decision carousel, so **Aprovar** is available when nothing was found.
