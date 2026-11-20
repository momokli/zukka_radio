# Build context: repository root.
#   docker build -f tapes/Dockerfile -t zukka-tapes:test .
# syntax=docker/dockerfile:1

FROM rust:1.85-bookworm AS builder
WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY scheduler/Cargo.toml scheduler/Cargo.toml
COPY portal/Cargo.toml portal/Cargo.toml
COPY tapes/Cargo.toml tapes/Cargo.toml
COPY scheduler ./scheduler
COPY portal ./portal
COPY tapes ./tapes

RUN cargo build --release --locked -p zukka-tapes

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /src/target/release/zukka-tapes /usr/local/bin/tapes

USER nonroot:nonroot
EXPOSE 8080

HEALTHCHECK --interval=15s --timeout=5s --retries=5 --start-period=30s \
    CMD ["/usr/local/bin/tapes", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/tapes"]