# Builds the Linux server engine: Horizon compiles only for the system it runs on.
FROM debian:trixie-slim

ARG LAUNCHER_TAG=launcher%400.2.58
ARG LAUNCHER_ASSET=horizon-launcher-linux-x86_64.tar.gz

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libasound2-dev libudev-dev pkg-config \
        libxcb1 libxkbcommon0 libxkbcommon-x11-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/launcher
RUN base="https://github.com/horizon-rust/horizon/releases/download/${LAUNCHER_TAG}" \
    && curl -fsSL -o "${LAUNCHER_ASSET}" "${base}/${LAUNCHER_ASSET}" \
    && curl -fsSL -o SHA256SUMS "${base}/SHA256SUMS" \
    && grep " ${LAUNCHER_ASSET}\$" SHA256SUMS | sha256sum -c - \
    && tar -xzf "${LAUNCHER_ASSET}" \
    && rm "${LAUNCHER_ASSET}" SHA256SUMS

COPY build-server.sh /usr/local/bin/build-server.sh
# A Windows checkout may have given the script CRLF line endings.
RUN sed -i 's/\r$//' /usr/local/bin/build-server.sh && chmod +x /usr/local/bin/build-server.sh

ENTRYPOINT ["/usr/local/bin/build-server.sh"]
