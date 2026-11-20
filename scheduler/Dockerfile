# Build context: repository root.
#   docker build -f scheduler/Dockerfile -t zukka-scheduler:test .
# syntax=docker/dockerfile:1

FROM rust:1.85-bookworm AS builder
WORKDIR /src

# Workspace manifests first (dependency layer), then sources.
COPY Cargo.toml Cargo.lock ./
COPY scheduler/Cargo.toml scheduler/Cargo.toml
COPY portal/Cargo.toml portal/Cargo.toml
COPY tapes/Cargo.toml tapes/Cargo.toml
COPY scheduler ./scheduler
COPY portal ./portal
COPY tapes ./tapes

RUN cargo build --release --locked -p zukka-scheduler

# Writable mount points pre-owned by the distroless nonroot user. Docker
# initialises an empty named volume with this ownership, so the scheduler can
# create `zukka.db` and rotation files without running as root.
RUN mkdir -p /volumes/data /volumes/hls /volumes/rotation /volumes/recordings \
    && chown -R 65532:65532 /volumes

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /src/target/release/zukka-scheduler /usr/local/bin/scheduler
COPY --from=builder --chown=65532:65532 /volumes/ /

USER nonroot:nonroot
EXPOSE 8080 9091

HEALTHCHECK --interval=10s --timeout=5s --retries=5 --start-period=30s \
    CMD ["/usr/local/bin/scheduler", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/scheduler"]