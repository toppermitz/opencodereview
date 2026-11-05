# AGENTS.md

Guide for contributors and AI coding agents working on OpenCodeReview. Read it before changing code. User-facing documentation lives in [README.md](README.md).

## Repository map

```text
src/, src/platform/                      React frontend: onboarding, reviews, indexing, skills, settings
backend/src/providers/                   AI CLIs: claude.rs (CLI arguments, tool policy), codex.rs, engines.rs (models)
backend/src/accounts/                    AI account credentials, plan usage, sign-in projection into runners
backend/src/sandbox/                     ocr-sandbox: runner, egress proxy, read commands, tool guard
backend/src/review/                      Review orchestration, scope, coverage, evidence, findings, validation
backend/src/review_archive/              Frozen review archives, retention and purge
backend/src/gitlab/, indexer/            GitLab calls through `glab`; repository indexer
backend/src/server/                      HTTP API, background jobs, publication, sign-in flows
backend/src/bench/                       ocr-bench: headless reviewer of the public benchmark image
backend/src/db/                          Postgres access, migrations (pg/), queries, SQLite import
backend/src/assets/                      Built-in review skill (review-skill.md)
docker-compose.yml, docker/              Docker runtime, Dockerfiles, smoke and regression scripts
bench/                                   Public benchmark adapters (ReviewBench, Martian, AACR-Bench) and reports
.agents/skills/                          Agent workflows: git, comments
```

## Agent commands

| Command | Workflow |
| --- | --- |
| `/git <intent>` | [.agents/skills/git/SKILL.md](.agents/skills/git/SKILL.md): commits, pushes and pull requests. |
| `/comments` | [.agents/skills/comments/SKILL.md](.agents/skills/comments/SKILL.md): resolve review feedback on the current branch's pull request. |

## Ground rules

- **pnpm only.** Do not add `package-lock.json`, `yarn.lock` or `bun.lock`.
- **Docker is the runtime.** OpenCodeReview runs only through `docker compose`. Do not add native desktop wrappers or non-Docker runtime configuration. The one exception is the benchmark image (`docker/Dockerfile.bench`, [bench/README.md](bench/README.md)): a single container that reviews one mounted pull request for public benchmarks and exits. It is not a product runtime and has no HTTP API or UI.
- **File size limits.** Frontend source files stay at or under 800 lines and Rust files at or under 2,500 lines (`pnpm check:file-size`). Split before you hit the limit.
- **Conventional Commits** for every commit and pull request title.
- **Pull requests only.** `main` is protected; every change lands through a squash-merged pull request reviewed by a code owner.
- **No AI attribution.** Do not add `Generated with` footers, AI `Co-Authored-By` trailers or agent signatures to commits, PR titles or PR bodies. OpenCodeReview's own finding markers in product code are not attribution and stay.
- **No secrets** in code, logs, docs, fixtures or commits. Never commit `.env` files or databases.

## Product rules

- OpenCodeReview is reviews, indexing and skills. Do not add unrelated product areas without a discussion in an issue first.
- Reviews run Claude Code (`claude --print`) or Codex (`codex exec`). Each provider's model and effort come from the list in `backend/src/providers/engines.rs`, picked in the top-bar engine picker (`EnginePicker`, stored as `review_engine_<provider>`). The first entry is the default: Claude Opus 5.5 at medium effort, and Codex's own default model at medium. Add models only to that list; do not add other providers without a product discussion.
- All AI CLIs are baked into the image at pinned, checksum-verified versions. Do not add runtime CLI installs or auto-updates.
- Accounts are tried in priority order, the first usable account's provider before the other (`pick_ai_account`, mirrored by `nextAccountId` in the UI); a usage limit rests the account and the same stage reruns on the next one.
- Usage is reported in tokens only. Do not add currency or cost estimates.
- GitLab access goes through the authenticated `glab` CLI. Do not add OpenCodeReview-managed GitLab tokens.
- Configuration lives in Settings (Providers, Repository access, PR selection, Integrations) and the top-bar engine picker. Review execution and publishing are otherwise fixed defaults, except whether suggestions are posted (PR selection → Review comments → Post suggestions, off by default). `review_lens_pass` and `review_focus_files` are stored settings with no Settings screen; only `ocr-bench` turns them on.
- The index is file metadata, a package and import graph and lightweight symbols. Symbol extraction is not an AST parser; do not describe it as one.
- Never check out or switch a user's branch for PR context. Use fetch, diff and show against refs.
- Long-running work never runs synchronously in a request. Use background jobs with visible state and actionable errors, and clean up timers, subprocesses and file handles.
- Posted findings keep the OpenCodeReview `<!-- ocr-... -->` markers and the full finding and question templates.
- The built-in review skill is `backend/src/assets/review-skill.md`. Improve it there; local database skill versions are not shipped.

## Sandbox invariants

- Runners get only their IPC socket and the read-only sign-in projection. Never mount `ocr-data` or repositories into a runner, never let a runner reach the database, and never expose credential bytes (database URLs included) to the browser or logs.
- All Claude CLI arguments live in `backend/src/providers/claude.rs`. Runners expose `Read`, `Grep`, `Glob`, `Bash` and `Write`; deny `Agent`, `Task`, `WebFetch`, `WebSearch` and `mcp__*`; and run the `ocr-sandbox tool-guard` `PreToolUse` hook on every call.
- Egress admits only the AI provider hosts in `backend/src/sandbox/egress.rs`.
- Backend and runner images share an executor fingerprint; deploy them together. Never add an in-process fallback that runs the AI inside `server`.
- Never automatically re-run inference after a startup failure. A successful process exit still requires report validation.
- In the benchmark image, `docker/bench-entrypoint.sh` starts Postgres and one `ocr-sandbox runner` in the same container, then `ocr-bench` takes the server's place and never runs the AI itself. The runner keeps its rules (UID drop, tool guard, sign-in projection only); `OCR_RUNNER_KEEP` only spares the benchmark's mounts under `/work` between stages. Keep one runner per container: the runner's survivor check covers every process of the model UID.
- Paid AI calls (real review diagnostics) always require explicit authorization from a maintainer.

## Review invariants

- Scope uses the live immediate base, head and merge-base. A failed fetch fails preparation; never fall back to stale refs or the default branch.
- Evidence is frozen and validated for every stage before any inference. A repository larger than the archive's budgets is frozen with the pull request's changed, related, test and convention files first and the rest in path order until the budgets are full; files past them are excluded (or, past the manifest budget, left out) and can never be cited. Findings must quote the snapshot exactly and anchor to changed lines; invalid reports fail, they are never relabelled clean. Lower-priority points (test gaps, stale names and docs, duplication, style) are suggestions: validated against the snapshot like findings, but listed in the review summary instead of posted inline, and never actionable on their own. Reviews always ask for them, so low-priority points never become findings, but unless Post suggestions is on the section is removed from the body a review stores and audits, before anything reads it. After output repair, a finding, general question or suggestion that still fails validation is dropped and reported as a warning while the valid ones stand. A chunk left with nothing valid drops those points too, so one chunk never fails the others' findings; any other report left with nothing valid still fails, and a review with a dropped point is never treated as clean. A candidate the final pass keeps `unchanged` posts the comment validated in its chunk, confirmed, so the final report repeats only the findings it changes or adds. A candidate the final pass leaves without a usable decision keeps the comment validated in its chunk, marked unconfirmed, and so does every candidate of a final batch whose output never validates. A quote's line may be miscounted by up to three lines: validation moves it to the one nearest line holding the exact text, and a tie or a farther quote still fails. A cited path missing its leading directories is completed only when exactly one file at that revision ends with it and holds the exact text there. A finding anchored to a line number of PR_DIFF.patch itself, instead of its file's, moves to the line that diff line adds or removes when it belongs to the same file.
- Output repair allows at most one extra execution per stage. It may run beside reported missing evidence. It only corrects lines, sides, quotes and empty counter-evidence, and except for a schema error it gets a short prompt and returns only those corrections, which are applied to the rejected report; anything else it changes is put back as first reported, and the result is validated as usual.
- With `review_lens_pass` on, each stage gets one lens execution after its report validates. The lens only adds findings, its report is validated on its own and again once merged, and any lens failure leaves the first report as it stands.
- A review with a time budget (only `ocr-bench` gives one) starts the lens, an output repair or a final integration batch only while the work it still requires fits before the deadline. Without time for a repair, the invalid points are dropped instead; a batch left out keeps its candidates' chunk-validated comments, marked unconfirmed. Without a budget every step runs.
- Publication is idempotent and bound to the exact validated selection. Nothing approves automatically.
- Archive retention deletes only frozen code of merged pull requests; never delete review results, audits, Git data or coverage.

## Runtime

```bash
docker compose up --build
```

Open `http://localhost:1421`. Services: `web` (nginx on 1421, proxying `/api`), `server` (API on `127.0.0.1:8787`), `review-runner` (20 replicas by default, `OCR_REVIEW_RUNNERS`), `review-egress`, `postgres` and `database-secret`. State lives in the `ocr-data` and `postgres-data` volumes. `GITLAB_TOKEN` is only a development fallback. `OCR_GITLAB_HOST` is required for the Self-Hosted instance.

## Development commands

```bash
pnpm install
pnpm debug:web                     # Vite on 1421 against the Docker backend
pnpm lint:fix                      # Biome: lint, imports, format; CI runs pnpm lint
pnpm knip                          # unused files, exports and dependencies
pnpm typecheck
pnpm test
pnpm build
pnpm check:file-size
cargo fmt --all                    # rustfmt; CI runs cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked    # or: cargo test -p opencodereview-server <filter> --lib --locked
cargo deny check                   # when Cargo.lock changes: advisories, licenses, sources
pnpm changelog:check               # release notes in changelog.d/
python3 -B docker/test_diagnostics.py
python3 -B docker/test-web-proxy.py
docker compose config --quiet
```

After building images, `python3 -u docker/runner-smoke.py` validates the sandbox with fake inference (no credentials, no paid calls). Rust tests need `OCR_TEST_DATABASE_URL` pointing at a Postgres where the role may create databases ([CONTRIBUTING.md](CONTRIBUTING.md)). Zero matching tests is not a pass.

## Database

Postgres. Migrations are the SQL files in `backend/src/db/pg/`, listed in `migrate.rs`. Append new migrations at the end; never edit an applied one. A database newer than the app fails explicitly. `backend/src/db/schema.sql` and its SQLite migrations remain only for `ocr-server import-sqlite`.

- Bind every value as a parameter; never format input into SQL.
- A transaction that reads a pull request's state to decide a write calls `lock_pull_request` first.
- Never hold a `Database` across Git, GitLab or model calls: open one per step.
- Background work that must not run twice when several server instances share the database goes through `Db::run_exclusive`.

## UI rules

- Style with Tailwind utilities and the theme tokens in `src/styles/globals.css`. No raw hex colors in components.
- Reuse the primitives in `src/components/DesignSystem.tsx` before adding new ones.
- `src/styles/components.css` is only for third-party internals (React Flow, rendered Markdown, TipTap). Do not add stylesheet rules for app components.
- Loading states use `src/components/BrandLoading.tsx`, not skeletons or generic spinners.
- Empty states say which action fills the area. Do not add placeholder tabs.
- At most two visible buttons per action area, primary last; secondary actions go in a `DSMenu`.
- An unreachable API shows actionable Docker guidance, not "Failed to fetch".

## Code style

- Match the existing architecture and naming. Prefer compact, idiomatic code with minimal nesting.
- Add comments only where the code needs them to be understood.
- Check existing dependencies before adding new ones.
- Vitest uses jsdom for frontend interaction tests. Keep `@types/node` explicit.

## Release notes

User-visible changes get a release note in `changelog.d/<branch>.md` ([changelog.d/README.md](changelog.d/README.md)); do not edit `CHANGELOG.md` directly. Internal changes get the `no-changelog` label instead.

## Before you finish

Run the checks for the scope you changed, check `git status --short`, and keep generated artifacts (`node_modules/`, `dist/`, `target/`) and runtime state (`*.sqlite`, `.env`) out of Git.
