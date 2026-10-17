# syntax=docker/dockerfile:1
#
# Images of pacod-id. Build from the repository root (or from a vendored copy's root, such as
# vendor/pacod-id: every path below is relative to it):
#
#   docker build -f deploy/docker/pacod-id.Dockerfile -t pacod-id .
#   docker build -f deploy/docker/pacod-id.Dockerfile --target mock-idp -t pacod-id-mock-idp .
#
# The build context is narrowed by pacod-id.Dockerfile.dockerignore (next to this file) to the
# Cargo workspace, the npm workspace manifests and the sources of the hosted pages.
#
# Stages:
#   pages-deps  npm ci for the design package and the hosted pages; rebuilt when a manifest
#               or the lock file changes.
#   pages       builds packages/ui, then ui/ (the SvelteKit pages, static).
#   planner     the cargo-chef recipe (manifests and lock file only).
#   deps        every third-party crate, compiled once for both binaries.
#   build-*     one binary each. The service also compiles in ui/messages (its mails) and
#               packages/ui/tokens (the mails' colours).
#
# The binaries are linked statically against musl and run on a distroless base without a
# shell or a package manager. mimalloc replaces the musl allocator in the service.

ARG RUST_VERSION=1.99.0
ARG CARGO_CHEF_VERSION=0.1.78
ARG NODE_VERSION=24.21.0
ARG ALPINE_VERSION=3.24
# The tag "nonroot" moves; the digest pins it. Refresh with:
#   docker buildx imagetools inspect gcr.io/distroless/static-debian13:nonroot
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian13:nonroot@sha256:e2e927ec666bae08560abb3c55d0659eceabb657f56b6782ab500a9fc7f555e3

# ---------------------------------------------------------------------------------------------
# The hosted pages. npm ci checks the lock file against every workspace it declares, so all
# workspace manifests come in, though only packages/ui and ui are built.
# ---------------------------------------------------------------------------------------------
FROM node:${NODE_VERSION}-alpine${ALPINE_VERSION} AS pages-deps
WORKDIR /src
COPY package.json package-lock.json .npmrc ./
COPY packages/authz/package.json packages/authz/
COPY packages/client/package.json packages/client/
COPY packages/sveltekit/package.json packages/sveltekit/
COPY packages/ui/package.json packages/ui/
COPY ui/package.json ui/
COPY fixtures/kit2/package.json fixtures/kit2/
COPY fixtures/kit3/package.json fixtures/kit3/
RUN --mount=type=cache,id=pacod-id-npm,target=/root/.npm \
    npm ci --workspace packages/ui --workspace ui --no-audit --no-fund

FROM pages-deps AS pages
COPY packages/ui packages/ui
COPY ui ui
RUN set -eu; \
    npm run build -w packages/ui; \
    npm run build -w ui; \
    test -f ui/build/index.html

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
    CARGO_NET_RETRY=5

WORKDIR /src

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo chef prepare --recipe-path /recipe.json

FROM chef AS deps
COPY --from=planner /recipe.json /recipe.json
RUN --mount=type=cache,id=pacod-id-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-id-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --release --locked --workspace --recipe-path /recipe.json

FROM deps AS sources
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# Compiled into the service: the mails' texts and colours.
COPY ui/messages ui/messages
COPY packages/ui/tokens packages/ui/tokens

FROM sources AS build-service
RUN --mount=type=cache,id=pacod-id-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-id-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin pacod-id; \
    install -D "target/$(cat /rust-target)/release/pacod-id" /out/pacod-id

FROM sources AS build-mock
RUN --mount=type=cache,id=pacod-id-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=pacod-id-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    set -eu; \
    cargo build --release --locked --workspace --bin pacod-id-mock-idp; \
    install -D "target/$(cat /rust-target)/release/pacod-id-mock-idp" /out/pacod-id-mock-idp

# ---------------------------------------------------------------------------------------------
# Health probe for Docker health checks: the runtime image has no shell, curl or wget.
# Kubernetes probes do not need it.
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
# mock-idp: stands in for Google in development and tests. Never deployed.
# ---------------------------------------------------------------------------------------------
FROM runtime AS mock-idp
LABEL org.opencontainers.image.title="pacod-id-mock-idp" \
      org.opencontainers.image.description="Mock OpenID provider standing in for Google (development only)"
COPY --from=build-mock /out/pacod-id-mock-idp /usr/local/bin/pacod-id-mock-idp
ENV PID_MOCK_LISTEN=0.0.0.0:7080
EXPOSE 7080
HEALTHCHECK --interval=5s --timeout=3s --start-period=5s --retries=5 \
  CMD ["/usr/local/bin/healthcheck", "tcp://127.0.0.1:7080"]
ENTRYPOINT ["/usr/local/bin/pacod-id-mock-idp"]

# ---------------------------------------------------------------------------------------------
# pacod-id (the default target): public 8070 (sign-in pages, /oidc, /ui-api), internal 8071
# (/internal/v1, /admin/v1, server-to-server token calls), metrics and probes 9170.
# The configuration file (realms and clients) is mounted at PID_CONFIG; secrets come from the
# environment, from <NAME>_FILE or from PID_SECRETS_DIR.
# ---------------------------------------------------------------------------------------------
FROM runtime AS pacod-id
LABEL org.opencontainers.image.title="pacod-id" \
      org.opencontainers.image.description="Sign-in, accounts and access for several products (OpenID Connect provider)"
COPY --from=build-service /out/pacod-id /usr/local/bin/pacod-id
COPY --from=pages /src/ui/build /usr/share/pacod-id/ui
ENV PID_CONFIG=/etc/pacod-id/pacod-id.toml \
    PID_UI_DIR=/usr/share/pacod-id/ui \
    PID_PUBLIC_LISTEN=0.0.0.0:8070 \
    PID_INTERNAL_LISTEN=0.0.0.0:8071 \
    PID_METRICS_LISTEN=0.0.0.0:9170
EXPOSE 8070 8071 9170
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=10s --timeout=4s --start-period=30s --retries=3 \
  CMD ["/usr/local/bin/healthcheck", "http://127.0.0.1:9170/readyz"]
ENTRYPOINT ["/usr/local/bin/pacod-id"]
CMD ["serve"]
