FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p pacod-gw-analytics

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/pacod-gw-analytics /usr/local/bin/pacod-gw-analytics
EXPOSE 8090
CMD ["pacod-gw-analytics"]
