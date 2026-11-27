FROM rust:1.96-slim-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /app --shell /usr/sbin/nologin magnet \
    && mkdir -p /app/data \
    && chown magnet:magnet /app/data
WORKDIR /app
COPY --from=build /src/target/release/magnet /usr/local/bin/magnet
COPY registry.yaml ./
USER magnet
VOLUME ["/app/data"]
ENV MAGNET_HOST=0.0.0.0 \
    MAGNET_PORT=8000
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${MAGNET_PORT} && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && head -n1 <&3 | grep -q ' 200 '"]
ENTRYPOINT ["magnet"]
CMD ["serve"]
