FROM rust:1.98.1-bookworm AS server
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
  pkg-config libssl-dev \
  && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
COPY assets ./assets
ARG CARGO_TARGET_CACHE=ocr-cargo-target
# Cargo reads these from the environment. Builds keep two jobs and full optimization; the CI
# smoke sets more jobs and opt-level 0, since it checks the sandbox, not speed.
ARG CARGO_BUILD_JOBS=2
ARG CARGO_PROFILE_RELEASE_OPT_LEVEL
RUN --mount=type=cache,id=ocr-cargo-registry,target=/usr/local/cargo/registry,sharing=locked --mount=type=cache,id=${CARGO_TARGET_CACHE},target=/app/target,sharing=locked cargo build --locked --release --bin ocr-server \
  && cp /app/target/release/ocr-server /usr/local/bin/

FROM debian:bookworm-slim
# Debian's security updates land after the base image is published; take them at build time.
RUN apt-get update \
  && apt-get upgrade -y --no-install-recommends \
  && apt-get install -y --no-install-recommends ca-certificates curl git bubblewrap \
  && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
ARG GLAB_VERSION=1.128.0
ARG GLAB_SHA256_AMD64=bc024819e7598b19429a199cd87f640d98b2b67416d8ad796f19e4260b57a7c7
ARG GLAB_SHA256_ARM64=60d0512085a8c1be7c5ef51aa7e4ee28213444cea0376b3260707d90489199ea
RUN case "$TARGETARCH" in \
      amd64) glab_arch=amd64; checksum="$GLAB_SHA256_AMD64" ;; \
      arm64) glab_arch=arm64; checksum="$GLAB_SHA256_ARM64" ;; \
      *) echo "Unsupported glab architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && tarball="glab_${GLAB_VERSION}_linux_${glab_arch}.tar.gz" \
  && curl -fsSL --retry 3 "https://gitlab.com/gitlab-org/cli/-/releases/v${GLAB_VERSION}/downloads/${tarball}" -o "/tmp/${tarball}" \
  && printf '%s  /tmp/%s\n' "$checksum" "$tarball" | sha256sum -c - \
  && tar -xzf "/tmp/${tarball}" -C /tmp \
  && install -m 0755 /tmp/bin/glab /usr/local/bin/glab \
  && rm -rf "/tmp/${tarball}" /tmp/bin \
  && glab version | grep -F "${GLAB_VERSION}"
LABEL ai.glab.cli.version="${GLAB_VERSION}"
ARG CLAUDE_CLI_VERSION=2.1.292
ARG CLAUDE_CLI_SHA256_AMD64=a967e7b1d8b4e47ee421d5433027880347952b0c0857abf880e2c942a4ec93b3
ARG CLAUDE_CLI_SHA256_ARM64=24caa9e6ff13bf227049a2626f1c816fc895023050f0ec3b12dbf14d897367e0
RUN case "$TARGETARCH" in \
      amd64) platform=linux-x64; checksum="$CLAUDE_CLI_SHA256_AMD64" ;; \
      arm64) platform=linux-arm64; checksum="$CLAUDE_CLI_SHA256_ARM64" ;; \
      *) echo "Unsupported Claude CLI architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && mkdir -p /opt/claude/bin \
  && curl -fsSL --retry 3 "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/${CLAUDE_CLI_VERSION}/${platform}/claude" -o /opt/claude/bin/claude \
  && printf '%s  /opt/claude/bin/claude\n' "$checksum" | sha256sum -c - \
  && chmod 0755 /opt/claude/bin/claude \
  && ln -s /opt/claude/bin/claude /usr/local/bin/claude \
  && test "$(DISABLE_AUTOUPDATER=1 /opt/claude/bin/claude --version | cut -d ' ' -f1)" = "$CLAUDE_CLI_VERSION"
LABEL ai.claude.cli.version="${CLAUDE_CLI_VERSION}"
ARG CODEX_CLI_VERSION=0.163.0
ARG CODEX_CLI_SHA256_AMD64=beb339e8ecd9ba057a6bb2dc81ad8ca474812edfbf0fc5056ca818797648dde4
ARG CODEX_CLI_SHA256_ARM64=a7261e65f4a96d8f4ca65a069c6cf2dc2c33defc341e48078acefda9461fffe4
# Codex runs its shell tool through this host for current models; without it every command fails.
ARG CODEX_CODE_MODE_HOST_SHA256_AMD64=314263128ba6592e7e807c265deeaf928a692b72dcbfeb5e0b20c9423b703529
ARG CODEX_CODE_MODE_HOST_SHA256_ARM64=284356aefb197f00b76b912efdfdbae8ccf3ea1f89b976dde064b7ccb43214ef
RUN case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl; checksum="$CODEX_CLI_SHA256_AMD64"; host_checksum="$CODEX_CODE_MODE_HOST_SHA256_AMD64" ;; \
      arm64) target=aarch64-unknown-linux-musl; checksum="$CODEX_CLI_SHA256_ARM64"; host_checksum="$CODEX_CODE_MODE_HOST_SHA256_ARM64" ;; \
      *) echo "Unsupported Codex CLI architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
  && mkdir -p /opt/codex/bin \
  && curl -fsSL --retry 3 "https://github.com/openai/codex/releases/download/rust-v${CODEX_CLI_VERSION}/codex-${target}.tar.gz" -o /tmp/codex.tar.gz \
  && printf '%s  /tmp/codex.tar.gz\n' "$checksum" | sha256sum -c - \
  && tar -xzf /tmp/codex.tar.gz -C /opt/codex/bin \
  && mv "/opt/codex/bin/codex-${target}" /opt/codex/bin/codex \
  && rm /tmp/codex.tar.gz \
  && curl -fsSL --retry 3 "https://github.com/openai/codex/releases/download/rust-v${CODEX_CLI_VERSION}/codex-code-mode-host-${target}.tar.gz" -o /tmp/codex-code-mode-host.tar.gz \
  && printf '%s  /tmp/codex-code-mode-host.tar.gz\n' "$host_checksum" | sha256sum -c - \
  && tar -xzf /tmp/codex-code-mode-host.tar.gz -C /opt/codex/bin \
  && mv "/opt/codex/bin/codex-code-mode-host-${target}" /opt/codex/bin/codex-code-mode-host \
  && rm /tmp/codex-code-mode-host.tar.gz \
  && chmod 0755 /opt/codex/bin/codex /opt/codex/bin/codex-code-mode-host \
  && ln -s /opt/codex/bin/codex /usr/local/bin/codex \
  && test "$(/opt/codex/bin/codex --version | cut -d ' ' -f2)" = "$CODEX_CLI_VERSION"
LABEL ai.codex.cli.version="${CODEX_CLI_VERSION}"
ENV DISABLE_AUTOUPDATER=1
WORKDIR /app
COPY --from=server /app/assets ./assets
COPY --from=server /usr/local/bin/ocr-server /usr/local/bin/ocr-server
ENV OCR_BIND=0.0.0.0:8787 \
  OCR_DATA_DIR=/data \
  OCR_REPOS_DIR=/data/repos
VOLUME ["/data"]
EXPOSE 8787
CMD ["ocr-server"]
