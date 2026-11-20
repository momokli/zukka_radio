# Build context: repository root.
#   docker build -f deploy/icecast/Dockerfile -t zukka-icecast:test .
# syntax=docker/dockerfile:1

FROM debian:bookworm-slim

# Pinned distribution version (bookworm, amd64): no floating `:latest`.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        icecast2=2.4.4-4+b1 \
        curl \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/icecast/icecast.xml /etc/icecast2/icecast.xml

EXPOSE 8000

HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
    CMD ["curl", "-fsS", "http://127.0.0.1:8000/status-json.xsl"]

CMD ["icecast2", "-c", "/etc/icecast2/icecast.xml"]