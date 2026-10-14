# The published crate owns the dependency lock and decoder arithmetic guard.
# Packaging changes do not retag or replace the scanner's 0.10.0 source.
FROM rust:1.97.0-bookworm@sha256:8fa55b2f3ddf97471ab6a767bfa3f37e6bad0986ba823e75fea57e2a2a5c3073 AS build
ENV CARGO_TARGET_DIR=/build/target
RUN test "$(rustc --version | cut -d ' ' -f 2)" = "1.97.0" \
    && cargo install qrcode-ai-scanner-cli --version 0.10.0 --locked --root /install --jobs 2 \
    && test "$(/install/bin/qrscan --version)" = "qrscan 0.10.0" \
    && mkdir /sources \
    && cp /usr/local/cargo/registry/cache/*/qrcode-ai-scanner-0.10.0.crate /sources/ \
    && cp /usr/local/cargo/registry/cache/*/qrcode-ai-scanner-cli-0.10.0.crate /sources/ \
    && echo '6feb3f3c27ac1e50775f9752e5524fca81b6319e07aa4b13f8e4136aa2724329  /sources/qrcode-ai-scanner-0.10.0.crate' | sha256sum -c - \
    && echo 'ad253eecca7727805c699f70eb8dbade697fbe4bed71470196d8f85cc321ef7c  /sources/qrcode-ai-scanner-cli-0.10.0.crate' | sha256sum -c -

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
LABEL org.opencontainers.image.title="QR Code AI Scanner" \
    org.opencontainers.image.description="Local QR and barcode decoding and scannability scoring. Mount an image and run qrscan; no hosted service or network access is needed." \
    org.opencontainers.image.version="0.10.0" \
    org.opencontainers.image.revision="2d3b577582979f0dc603b5cb0d96d1be02b459f5" \
    org.opencontainers.image.source="https://github.com/supernovae-st/qrcode-ai-scanner" \
    org.opencontainers.image.url="https://qrcode-ai.com" \
    org.opencontainers.image.documentation="https://github.com/supernovae-st/qrcode-ai-scanner/tree/main/distribution/docker" \
    org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /install/bin/qrscan /usr/local/bin/qrscan
COPY --from=build /sources/ /usr/share/doc/qrscan/source/
COPY LICENSE LICENSING.md /usr/share/doc/qrscan/
COPY distribution/docker/SOURCE.md /usr/share/doc/qrscan/SOURCE.md
USER 65532:65532
WORKDIR /data
ENV RAYON_NUM_THREADS=2
ENTRYPOINT ["/usr/local/bin/qrscan"]
CMD ["--help"]
