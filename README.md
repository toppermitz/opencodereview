<p align="center">
  <img src="assets/mascot.svg" alt="OpenCodeReview mascot" width="128" height="128">
</p>

# OpenCodeReview

[![License](https://img.shields.io/badge/license-PolyForm_Small_Business_1.0.0-blue.svg)](LICENSE)

AI code review for GitLab Self-Hosted merge requests that runs on your own machine, using the Claude or ChatGPT account you already have.

OpenCodeReview watches the repositories you pick, reviews each merge request with [Claude Code](https://docs.anthropic.com/en/docs/claude-code) or [Codex](https://github.com/openai/codex) inside isolated Docker containers, checks every finding against the code, and posts the result on the merge request.

![The OpenCodeReview review queue](site/app-reviews.png)

## Features

- **Your own AI.** Reviews run on the Claude Code CLI (Claude Opus 5.5 at medium effort by default) or the Codex CLI, signed in with your Claude or ChatGPT account. The top bar picks each provider's model and effort. There is no OpenCodeReview account.
- **Several accounts with failover.** Connect more than one account, of any provider, and pick the one new reviews start on. When an account hits its usage limit, the review continues on the next account of the same provider, then the other provider, and the limited account comes back after its window resets. Settings shows how much of each Claude and ChatGPT plan is left.
- **Codebase indexing.** Each selected repository is indexed locally: file metadata, a package and import graph, and lightweight symbols. Reviews get the callers, dependencies and tests of the changed code, plus your convention files (`AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`, `REVIEW.md` and others).
- **Usage.** See the tokens reviews used by day, merge request, repository, model and account, with findings, time and cache reuse. A merge request opens into each of its runs.
- **Review skills.** The review methodology is a Markdown skill you can edit, version and roll back in the app.
- **Evidence-checked findings.** Every finding must quote the exact code it is about and anchor to a changed line. Reports that fail validation are never published as clean.
- **Incremental and stacked MRs.** New pushes review only what changed since the last review; stacked MRs are reviewed against their immediate base.
- **Isolated execution.** The AI CLI runs in locked-down runner containers with a read-only snapshot of the code, no repository checkout, and network access limited to your AI provider.
- **Guided onboarding** at `http://localhost:1421`: sign in to GitLab, connect your plan, pick repositories and run a first review.

## Quick start

```bash
git clone https://github.com/BridgLabs/opencodereview && cd opencodereview && docker compose up --build
```

Open [http://localhost:1421](http://localhost:1421) and follow onboarding:

1. **GitLab**: authorize with a device code against your Self-Hosted instance (`OCR_GITLAB_HOST` and `OCR_GITLAB_CLIENT_ID`).
2. **Your plan**: connect at least one account. Claude uses `claude setup-token` (you paste back the code it shows); ChatGPT uses the `codex login` browser flow or a device code.
3. **Repositories**: choose the repositories to monitor. They are cloned and indexed in the background.
4. **First review**: pick an open merge request. After that, new merge requests in monitored repositories are reviewed automatically.

The first build compiles the Rust backend and can take several minutes. To skip it, run the images published with each release instead:

```bash
docker compose -f docker-compose.yml -f docker-compose.images.yml up -d
```

Set `OCR_VERSION` (for example `OCR_VERSION=0.1.0`) to stay on one release; the default is the newest.

## Requirements

- Docker Engine or Docker Desktop with Docker Compose v2 (the Compose file uses `additional_contexts`).
- A GitLab Self-Hosted account with access to the repositories you want reviewed, plus `OCR_GITLAB_HOST` and `OCR_GITLAB_CLIENT_ID` (OAuth Application ID with Device authorization).
- At least one of:
  - a Claude subscription that works with Claude Code (Pro, Max, Team or Enterprise);
  - a ChatGPT plan that includes Codex.

Local models through Ollama or similar are not supported: the review egress proxy refuses private networks by design.

You do not need Node, pnpm or Rust on the host to run OpenCodeReview. Every AI CLI ships in the image at a pinned, checksum-verified version.

## Configuration

Everything else is configured in the app under **Settings** (providers, repository access, PR selection, integrations). Docker Compose reads these variables from your shell or from a `.env` file next to `docker-compose.yml`:

| Variable | Default | Purpose |
| --- | --- | --- |
| `OCR_GITLAB_HOST` | required | GitLab Self-Hosted host, as a bare `host` or `host:port` (for example `gitlab.example.com`). |
| `OCR_GITLAB_CLIENT_ID` | required | Application ID of a non-confidential OAuth app on that instance with Device authorization enabled (see below). |
| `OCR_WEB_APP_URL` | `http://localhost:1421` | The origin the UI is served from. Requests from other origins are rejected; change it when you open the UI from another address. |
| `OCR_REVIEW_RUNNERS` | `20` | Review runner containers; idle ones use almost no memory. |
| `OCR_VERSION` | `latest` | Image tag used with `docker-compose.images.yml`, for example `0.1.0`. |
| `OCR_REVIEW_DEBUG_DIR` | empty | Path inside the server container (for example `/data/review-debug`) where private review debug artifacts, which contain pull request code, are written. Leave empty in normal use. |

### GitLab OAuth application (once per instance)

Create an OAuth application on your GitLab Self-Hosted instance (User → Edit profile → Applications, or Admin → Applications):

- **Name:** `OpenCodeReview` (or similar)
- **Redirect URI:** `http://127.0.0.1/` (required by GitLab’s form; the device flow never redirects here)
- **Confidential:** unchecked
- **Device authorization:** enabled
- **Scopes:** `api`, `read_api`, `read_user`, `write_repository`, `openid`, `profile`

Copy the Application ID into `OCR_GITLAB_CLIENT_ID`. Ignore the Client Secret.

For example:

```bash
OCR_GITLAB_HOST=gitlab.example.com OCR_GITLAB_CLIENT_ID=your-application-id docker compose up --build
```

## How it works

```text
browser ──> web (nginx, :1421) ──/api──> server (Rust API, 127.0.0.1:8787)
                                            │  Git cache, glab CLI ──> postgres
                                            │  private IPC
                                            v
                              review-runner × 20 ──> review-egress ──> AI provider
```

| Service | Role |
| --- | --- |
| `web` | Serves the React UI and proxies `/api` to `server`. |
| `server` | Rust HTTP API: GitLab polling through the `glab` CLI, indexing, review orchestration, publication and background jobs. |
| `review-runner` | Runs Claude Code or Codex on a frozen snapshot of the merge request, one review stage per container at a time. 20 replicas by default (`OCR_REVIEW_RUNNERS`); idle ones use almost no memory. Each receives only a private socket and a read-only copy of the sign-in. |
| `review-egress` | Outbound proxy that allows only `api.anthropic.com`, `chatgpt.com`, `api.openai.com` and `auth.openai.com`, on port 443 and public addresses only. |

A review fetches the live base and head of the merge request, selects the scope (full, incremental, base sync or stacked), builds context from the index, freezes the evidence into an archive, runs the AI in a runner (split into chunks for large MRs), validates the report and publishes it. On GitLab, OpenCodeReview posts a summary note that it updates on each push, a result note that replaces the previous one, and the validated findings as merge request discussions with inline comments. Approving or requesting changes uses GitLab quick actions (`/approve`, `/request_changes`) on that note. Clean reviews wait for a human decision unless Settings → Auto-aprovar MRs limpos is on and the risk score is within the limit. A later review resolves OpenCodeReview discussions that the new result no longer keeps.

## Security and privacy

- **Your code goes only to your AI provider.** The merge request evidence is sent to Anthropic or OpenAI with your own sign-in, the same way their CLIs work when you use them directly. OpenCodeReview has no servers and sends no telemetry.
- **Sign-ins stay local.** GitLab credentials, AI sign-ins and keys are stored in the `ocr-data` Docker volume (AI sign-ins and keys with file mode `0600`). They are never logged or sent to the browser. **Sign out** (GitLab, in Integrations) and **Remove account** (AI accounts, in Providers) delete them.
- **Runners are sandboxed.** Read-only root filesystem, no Linux capabilities beyond what supervision needs, an unprivileged user for the AI process, no access to the database or repository cache, and provider-only network access.
- **The UI is meant for a trusted network.** The API is protected by an installation token that the web container injects into the UI, so anyone who can open the UI can use it. Compose publishes port 1421 on every network interface of the host, and Docker's port rules bypass host firewalls such as ufw. On a shared network or a VPS, change the mapping to `127.0.0.1:1421:1421` and reach it through an SSH tunnel, or put an authenticated reverse proxy in front.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Development

```bash
pnpm install
pnpm debug:web        # Vite on :1421 against the Docker backend
pnpm typecheck
pnpm test
pnpm build
cargo test --workspace --locked
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full setup and checks.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request, and follow the [Code of Conduct](CODE_OF_CONDUCT.md). Changes are tracked in [CHANGELOG.md](CHANGELOG.md).

## License and attribution

Copyright 2026 BRIDG LABS LTDA.

Licensed under the [PolyForm Small Business License 1.0.0](LICENSE). Individuals and companies with fewer than 100 people (employees and contractors) and less than USD 1,000,000 in revenue in the prior tax year (2019 dollars, adjusted for inflation) may use, modify and distribute it for free. Larger companies, counted together with the organizations they control or are controlled by, need a separate commercial license from the copyright owner. Personal, non-commercial use and use by non-profit organizations are also free, whatever their size, as [NOTICE](NOTICE) states. For a commercial license, or help rolling it out across a company, contact [contato@bridglabs.com](mailto:contato@bridglabs.com).

See [NOTICE](NOTICE) for attribution and third-party notices.

OpenCodeReview is an independent source-available project and is not affiliated with, endorsed by, or sponsored by Anthropic or OpenAI. Their product names are used only to describe compatibility.
