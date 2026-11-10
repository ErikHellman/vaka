# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p vaka-server -p vaka && \
    cp target/release/vaka-server target/release/vaka /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data vaka && mkdir -p /data && chown vaka /data
COPY --from=build /usr/local/bin/vaka-server /usr/local/bin/vaka /usr/local/bin/
USER vaka
ENV VAKA_DATA_DIR=/data VAKA_LISTEN=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD curl -fs http://localhost:8080/health || exit 1
ENTRYPOINT ["vaka-server"]
