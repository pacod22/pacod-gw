# syntax=docker/dockerfile:1
#
# Images for the Rust binaries of the workspace. Build from the repository root:
#
#   docker build -f deploy/docker/rust.Dockerfile --target core      -t pacod-gw-core .
#   docker build -f deploy/docker/rust.Dockerfile --target analytics -t pacod-gw-analytics .
#   docker build -f deploy/docker/rust.Dockerfile --target lite      -t pacod-gw-lite .
#   docker build -f deploy/docker/rust.Dockerfile --target mock      -t pacod-gw-mock .
#
# The build context is narrowed by rust.Dockerfile.dockerignore (next to this file) to the
# Cargo workspace, so changes to the UI, the docs or deploy/ do not invalidate any layer.
#
# Layer caching:
#   planner  writes the cargo-chef recipe (manifests and lock file only).
#   deps     compiles every third-party crate of the workspace once. This layer is shared by
#            the three targets and is rebuilt only when a Cargo.toml or Cargo.lock changes.
#   build-*  compiles the workspace crates one binary needs. A broken crate that a binary
#            does not depend on does not break that binary's image.
# The cargo registry and git checkouts are BuildKit cache mounts, so a changed lock file
# downloads only what is new. target/ stays in the layers on purpose: registry-backed layer
# caches (CI) can store layers but not cache mounts.
#
# The binaries are linked statically against musl and run on a distroless base without a
# shell or a package manager. mimalloc (a workspace dependency) replaces the musl allocator.

ARG RUST_VERSION=1.99.0
ARG CARGO_CHEF_VERSION=0.1.78
# The tag "nonroot" moves; the digest pins it. Refresh with:
#   docker buildx imagetools inspect gcr.io/distroless/static-debian13:nonroot
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian13:nonroot@sha256:e2e927ec666bae08560abb3c55d0659eceabb657f56b6782ab500a9fc7f555e3

# ---------------------------------------------------------------------------------------------
# Toolchain: Rust, cargo-chef and the musl target of the platform being built.
# ---------------------------------------------------------------------------------------------
FROM lukemathwalker/cargo-chef:${CARGO_CHEF_VERSION}-rust-${RUST_VERSION}-slim-trixie AS chef

RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*

# Every cargo call below builds for <arch>-unknown-linux-musl. The triple is kept in a file
# because an ENV instruction cannot hold the result of a command.
RUN set -eu; \
    triple="$(uname -m)-unknown-linux-musl"; \
    rustup target add "$triple"; \
    printf '%s' "$triple" > /rust-target; \
    printf '[build]\ntarget = "%s"\n' "$triple" >> "$CARGO_HOME/config.toml"

ENV CARGO_INCREMENTAL=0 \
    CARGO_TERM_COLOR=never \
    CARGO_NET_RETRY=5 \
    # sqlx query macros read the checked-in .sqlx/ data and never open a database connection.
    SQLX_OFFLINE=true

WORKDIR /src

# ---------------------------------------------------------------------------------------------
# Recipe: the dependency graph without the sources.
# ---------------------------------------------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path /recipe.json

# ---------------------------------------------------------------------------------------------
# Third-party crates, compiled once for the whole workspace.
# ---------------------------------------------------------------------------------------------
FROM chef AS deps
COPY --from=planner /recipe.json /recipe.json
# The recipe leaves out path dependencies outside the workspace: pacod-id's vendored crates
# come as they are (they change only when vendored again).
COPY vendor/pacod-id/crates /src/vendor/pacod-id/crates
RUN --mount=type=cache,id=pacod-gw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-gw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --release --locked --workspace --recipe-path /recipe.json

# ---------------------------------------------------------------------------------------------
# One build stage per binary. "--workspace --bin <name>" keeps the feature set that the deps
# stage compiled, so nothing third-party is compiled twice, and builds only that binary.
# ---------------------------------------------------------------------------------------------
FROM deps AS build-core
COPY . .
RUN --mount=type=cache,id=pacod-gw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-gw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin pacod-gw-core; \
    install -D "target/$(cat /rust-target)/release/pacod-gw-core" /out/pacod-gw-core; \
    mkdir -p /out/spool

FROM deps AS build-analytics
COPY . .
RUN --mount=type=cache,id=pacod-gw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-gw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin pacod-gw-analytics; \
    install -D "target/$(cat /rust-target)/release/pacod-gw-analytics" /out/pacod-gw-analytics

FROM deps AS build-lite
COPY . .
RUN --mount=type=cache,id=pacod-gw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-gw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin pacod-gw-lite; \
    install -D "target/$(cat /rust-target)/release/pacod-gw-lite" /out/pacod-gw-lite

FROM deps AS build-mock
COPY . .
RUN --mount=type=cache,id=pacod-gw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-gw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin gw-mock; \
    install -D "target/$(cat /rust-target)/release/gw-mock" /out/gw-mock

# ---------------------------------------------------------------------------------------------
# Health probe for Docker health checks. The runtime images have no shell, curl or wget, so
# this std-only program (about 400 kB) does the HTTP GET. Kubernetes probes do not need it.
# ---------------------------------------------------------------------------------------------
FROM chef AS healthcheck
COPY deploy/docker/healthcheck.rs /src/healthcheck.rs
RUN set -eu; \
    mkdir -p /out; \
    rustc --edition 2024 --target "$(cat /rust-target)" \
      -C opt-level=s -C panic=abort -C codegen-units=1 -C strip=symbols \
      -o /out/healthcheck /src/healthcheck.rs

# ---------------------------------------------------------------------------------------------
# Runtime base: CA certificates, time zone data, /etc/passwd with the user "nonroot" (65532).
# ---------------------------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime
ARG VERSION=dev
ARG VCS_REF=unknown
LABEL org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"
COPY --from=healthcheck /out/healthcheck /usr/local/bin/healthcheck
USER 65532:65532
WORKDIR /

# ---------------------------------------------------------------------------------------------
# core: data plane 8080, management API 8081, metrics 9100.
# ---------------------------------------------------------------------------------------------
FROM runtime AS core
LABEL org.opencontainers.image.title="pacod-gw-core" \
      org.opencontainers.image.description="pacod-gw data plane and management API"
COPY --from=build-core /out/pacod-gw-core /usr/local/bin/pacod-gw-core
# The spool directory exists in the image and belongs to the runtime user, so a named volume
# mounted here for the first time takes over that ownership.
COPY --from=build-core --chown=65532:65532 /out/spool /var/lib/pacod-gw/spool
ENV PGW_LISTEN=0.0.0.0:8080 \
    PGW_ADMIN_LISTEN=0.0.0.0:8081 \
    PGW_METRICS_LISTEN=0.0.0.0:9100 \
    PGW_SPOOL_DIR=/var/lib/pacod-gw/spool
VOLUME ["/var/lib/pacod-gw/spool"]
EXPOSE 8080 8081 9100
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pacod-gw-core"]
CMD ["serve"]

# ---------------------------------------------------------------------------------------------
# analytics: internal API 8090.
# ---------------------------------------------------------------------------------------------
FROM runtime AS analytics
LABEL org.opencontainers.image.title="pacod-gw-analytics" \
      org.opencontainers.image.description="pacod-gw event ingest, reports and batch jobs"
COPY --from=build-analytics /out/pacod-gw-analytics /usr/local/bin/pacod-gw-analytics
ENV ANALYTICS_LISTEN=0.0.0.0:8090
EXPOSE 8090
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pacod-gw-analytics"]

# ---------------------------------------------------------------------------------------------
# lite: the minimal gateway (docs/lite.md). Data plane 8080 only; configured by environment
# variables; writes nothing, so the root file system can stay read-only.
# ---------------------------------------------------------------------------------------------
FROM runtime AS lite
LABEL org.opencontainers.image.title="pacod-gw-lite" \
      org.opencontainers.image.description="pacod-gw data plane on its own: no database, configured by environment variables"
COPY --from=build-lite /out/pacod-gw-lite /usr/local/bin/pacod-gw-lite
ENV PGW_LISTEN=0.0.0.0:8080
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/pacod-gw-lite"]
CMD ["serve"]

# ---------------------------------------------------------------------------------------------
# mock: scripted LLM provider for the development stack. Never deployed.
# ---------------------------------------------------------------------------------------------
FROM runtime AS mock
LABEL org.opencontainers.image.title="pacod-gw-mock" \
      org.opencontainers.image.description="Mock LLM provider for the pacod-gw development stack"
COPY --from=build-mock /out/gw-mock /usr/local/bin/gw-mock
EXPOSE 9000
ENTRYPOINT ["/usr/local/bin/gw-mock"]
CMD ["--listen", "0.0.0.0:9000"]
