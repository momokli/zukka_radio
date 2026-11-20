# Build context: repository root.
#   docker build -f portal/Dockerfile -t zukka-portal:test .
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

RUN cargo build --release --locked -p zukka-portal

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /src/target/release/zukka-portal /usr/local/bin/portal

USER nonroot:nonroot
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=5s --retries=5 --start-period=30s \
    CMD ["/usr/local/bin/portal", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/portal"]