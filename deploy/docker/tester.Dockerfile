# syntax=docker/dockerfile:1
#
# Image for the tester: the Python engine that runs test cases against the gateway, started
# from the console's Testing page. Build from the repository root:
#
#   docker build -f deploy/docker/tester.Dockerfile -t pacod-gw-tester .
#
# The build context is narrowed by tester.Dockerfile.dockerignore (next to this file) to
# tester/ and vendor/pacod-id/python/. Dependencies come from tester/uv.lock.

ARG PYTHON_VERSION=3.13
ARG UV_VERSION=0.11.8

FROM ghcr.io/astral-sh/uv:${UV_VERSION} AS uv

FROM python:${PYTHON_VERSION}-slim AS build
COPY --from=uv /uv /usr/local/bin/uv
ENV UV_COMPILE_BYTECODE=1 \
    UV_LINK_MODE=copy \
    UV_PYTHON_DOWNLOADS=never
WORKDIR /app
# Dependencies first: the layer is reused until the lock file changes.
# pacod-id's client, where tester/pyproject.toml finds it (../vendor/pacod-id/python).
COPY vendor/pacod-id/python /vendor/pacod-id/python
COPY tester/pyproject.toml tester/uv.lock ./
RUN --mount=type=cache,id=pacod-gw-uv,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-install-project
COPY tester/ ./
RUN --mount=type=cache,id=pacod-gw-uv,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-editable

FROM python:${PYTHON_VERSION}-slim AS tester
ARG VERSION=dev
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="pacod-gw-tester" \
      org.opencontainers.image.description="pacod-gw test engine" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"
# /data holds the run history and uploaded suites. A new named volume takes the owner of the
# directory from the image, so the unprivileged user can write to it.
RUN mkdir /data && chown 65532:65532 /data
COPY --from=build /app/.venv /app/.venv
ENV PATH=/app/.venv/bin:$PATH \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    TESTER_DATA_DIR=/data \
    TESTER_LISTEN=0.0.0.0:8095
USER 65532:65532
EXPOSE 8095
STOPSIGNAL SIGTERM
CMD ["python", "-m", "pgw_tester"]
