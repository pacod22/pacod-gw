# syntax=docker/dockerfile:1
#
# Image for the admin console (SvelteKit 3, adapter-node 6). Build from the repository root:
#
#   docker build -f deploy/docker/ui.Dockerfile -t pacod-gw-ui .
#
# The build context is narrowed by ui.Dockerfile.dockerignore (next to this file) to ui/.
#
# SvelteKit 3 needs Node 22.17 or newer. Node 24 is the long-term-support line.

ARG NODE_VERSION=24.21.0
ARG ALPINE_VERSION=3.24

FROM node:${NODE_VERSION}-alpine${ALPINE_VERSION} AS base
WORKDIR /app

# All dependencies, for the build. The layer is reused until the lock file changes.
FROM base AS deps
COPY ui/package.json ui/package-lock.json ui/.npmrc ./
# pacod-id's packages (@pacod/ui, @pacod/id-authz), packed by scripts/vendor-pacod-id.sh.
COPY ui/vendor ./vendor
RUN --mount=type=cache,id=pacod-gw-npm,target=/root/.npm \
    npm ci

FROM deps AS build
COPY ui/ ./
RUN npm run build

# Runtime dependencies only. adapter-node bundles devDependencies into build/, so this is
# empty unless package.json lists "dependencies". mkdir keeps the COPY below valid either way.
FROM base AS prod-deps
COPY ui/package.json ui/package-lock.json ui/.npmrc ./
COPY ui/vendor ./vendor
RUN --mount=type=cache,id=pacod-gw-npm,target=/root/.npm \
    npm ci --omit=dev \
 && mkdir -p node_modules

FROM base AS ui
ARG VERSION=dev
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="pacod-gw-ui" \
      org.opencontainers.image.description="pacod-gw admin console" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"
ENV NODE_ENV=production \
    HOST=0.0.0.0 \
    PORT=3000
COPY --from=prod-deps --chown=node:node /app/node_modules ./node_modules
COPY --from=build --chown=node:node /app/build ./build
COPY --chown=node:node ui/package.json ./
USER node
EXPOSE 3000
STOPSIGNAL SIGTERM
# Exec form: node is PID 1 and receives SIGTERM, so adapter-node shuts down gracefully.
CMD ["node", "build"]
