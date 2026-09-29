# The gate's Linux environment: the pinned rust image plus the pinned
# cargo-nextest, so the container modes can run the same `test` mode CI runs.
# `scripts/ci.sh linux` and `linux-amd64` build and use it; nothing else does.
#
# The base is pinned by its OCI index digest, because a tag can be re-cut under
# the repository between two runs, exactly like an action tag. Digest verified
# against the registry on 2026-09-26:
#   docker buildx imagetools inspect rust:1.94.0-bookworm
FROM rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f

# One pinned pair per architecture the gate runs on: the CI runners' x86_64 and
# an Apple silicon Mac's arm64. An unknown architecture fails the build rather
# than quietly producing an image whose test mode cannot run. The version is
# the one .config/nextest.toml requires, and each binary is verified against
# the sha256 the release publishes next to it.
ARG TARGETARCH
ARG NEXTEST_VERSION=0.9.146

RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; \
             sha=682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428 ;; \
      arm64) triple=aarch64-unknown-linux-gnu; \
             sha=b2e33d7c72de7ade0ff7b3a948ac37516b24f8a836b7a8870c1f634a94be9de9 ;; \
      *)     echo "no pinned cargo-nextest for TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-${triple}.tar.gz"; \
    curl -fsSL -o /tmp/nextest.tar.gz "$url"; \
    echo "$sha  /tmp/nextest.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/nextest.tar.gz -C /usr/local/bin; \
    rm /tmp/nextest.tar.gz; \
    cargo nextest --version
