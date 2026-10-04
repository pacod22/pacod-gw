FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p pacod-gw-core

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/pacod-gw-core /usr/local/bin/pacod-gw-core
COPY config/gateway.example.toml /etc/pacod-gw/gateway.toml
ENV PACOD_GW_CONFIG=/etc/pacod-gw/gateway.toml
EXPOSE 8080 8081 9100
CMD ["pacod-gw-core"]
