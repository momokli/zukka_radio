#!/usr/bin/env bash
# Build and push the four Zukka images for a given Git SHA.
#
#   ./scripts/build-images.sh <sha>
#
# Registry defaults to ghcr.io/momokli; override with ZUKKA_IMAGE_REGISTRY.
# Build context is the repository root (see the Dockerfiles).
set -euo pipefail

readonly SHA_RE='^[0-9a-f]{7,40}$'

usage() {
    echo "usage: $0 <sha>" >&2
}

if [[ $# -ne 1 ]]; then
    usage
    exit 2
fi

sha="$1"
if [[ ! "$sha" =~ $SHA_RE ]]; then
    echo "invalid sha: '${sha}' (expected ${SHA_RE})" >&2
    exit 2
fi

registry="${ZUKKA_IMAGE_REGISTRY:-ghcr.io/momokli}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

build() {
    local name="$1"
    local dockerfile="$2"
    local image="${registry}/zukka-${name}:${sha}"
    echo "==> build ${image}"
    docker build -f "${repo_root}/${dockerfile}" -t "${image}" "${repo_root}"
}

push() {
    local name="$1"
    local image="${registry}/zukka-${name}:${sha}"
    echo "==> push ${image}"
    docker push "${image}"
}

build scheduler scheduler/Dockerfile
build portal portal/Dockerfile
build tapes tapes/Dockerfile
build icecast deploy/icecast/Dockerfile

push scheduler
push portal
push tapes
push icecast

echo "==> done: ${registry}/zukka-{scheduler,portal,tapes,icecast}:${sha}"