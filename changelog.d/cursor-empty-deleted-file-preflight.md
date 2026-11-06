### Fixed

- A review no longer fails preflight when the merge request deletes an empty file.
  Git omits the diff hunk for an empty blob, and those paths were reported as absent
  from the snapshot before any inference started.
