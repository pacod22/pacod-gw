# syntax=docker/dockerfile:1
#
# Image for gw-browser, the price tracker's browser: Debian's Chromium in headless mode. Build
# from the repository root:
#
#   docker build -f deploy/docker/browser.Dockerfile -t pacod-gw-browser .
#
# Chromium's headless mode is the browser people use, without a window. The headless shell
# (chromedp/headless-shell, for one) is a separate, stripped build, and a page tells it from a
# browser at once: no window.chrome, no plugins, permissions that disagree.

ARG DEBIAN_IMAGE=debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

FROM ${DEBIAN_IMAGE} AS browser
# Empty: the newest Chromium of Debian's security archive, which keeps no older one. A browser
# that renders other people's pages is worth keeping current; rebuild the image to update it.
ARG CHROMIUM_VERSION=
# The day of the build, which "make images" passes: the layer cache would otherwise keep the
# Chromium of the first build, and a build on a later day fetches the newest one again.
ARG CHROMIUM_DAY=
ARG VERSION=dev
ARG VCS_REF=unknown
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        chromium${CHROMIUM_VERSION:+=$CHROMIUM_VERSION} chromium-common${CHROMIUM_VERSION:+=$CHROMIUM_VERSION} \
        socat fonts-liberation fonts-dejavu-core fonts-noto-core fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/* \
    && dpkg-query -W -f '${Version}\n' chromium > /etc/chromium-version
# The user agent Chromium gives in headless mode, with "HeadlessChrome" written as "Chrome": what
# Chromium sends with a window. Asked of the browser itself, so that it follows the version.
RUN /usr/lib/chromium/chromium --headless --no-sandbox --user-data-dir=/tmp/ua --dump-dom \
        'data:text/html,<title></title><script>document.title = navigator.userAgent</script>' 2>/dev/null \
        | sed -n 's|.*<title>\(.*\)</title>.*|\1|p' | sed 's|HeadlessChrome/|Chrome/|' > /etc/chromium-user-agent \
    && rm -rf /tmp/ua \
    && grep -q ' Chrome/' /etc/chromium-user-agent && ! grep -q Headless /etc/chromium-user-agent
COPY --chmod=755 <<'EOF' /usr/local/bin/browser
#!/bin/sh
# Chromium serves its DevTools on loopback only; socat makes them reachable on the container's
# network, as the headless shell's image did. The profile lives on the scratch /tmp: Chromium
# refuses remote debugging of its default one.
#
# A page is to meet the browser a person would use. The user agent is set for the whole browser,
# so that a page's frames and workers, which are targets of their own, send it too; the client
# hints stay Chromium's own. The screen is a desktop's with a top bar (headless mode's is
# 800 x 600, all of it free), and the scraper maximises every page's window on it.
#
# Nothing of Chromium's own (updates, safe browsing, metrics) goes out: the proxy would let it
# through to the internet.
set -e
socat TCP-LISTEN:9222,fork,reuseaddr TCP:127.0.0.1:9223 &
exec /usr/lib/chromium/chromium \
  --headless \
  --no-sandbox \
  --remote-debugging-port=9223 \
  --user-data-dir=/tmp/chromium \
  "--user-agent=$(cat /etc/chromium-user-agent)" \
  "--screen-info={1920x1080 workAreaTop=32}" \
  --lang=en-US \
  --disable-background-networking \
  --disable-component-update \
  --no-first-run \
  "$@"
EOF
LABEL org.opencontainers.image.title="pacod-gw-browser" \
      org.opencontainers.image.description="Headless Chromium for the pacod-gw price tracker" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"
USER 65534:65534
ENV HOME=/tmp
EXPOSE 9222
ENTRYPOINT ["/usr/local/bin/browser"]
