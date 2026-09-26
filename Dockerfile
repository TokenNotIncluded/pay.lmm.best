FROM rust:slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler build-essential && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home pay && mkdir -p /var/lib/pay /etc/pay.lmm.best && chown pay:pay /var/lib/pay
COPY --from=build /app/target/release/pay-lmm /usr/local/bin/pay-lmm
USER 10001:10001
WORKDIR /var/lib/pay
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/pay-lmm"]
CMD ["--config", "/etc/pay.lmm.best/config.toml"]
