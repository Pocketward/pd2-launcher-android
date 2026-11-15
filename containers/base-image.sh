# Layer 1 of the build: the base image (Containerfile.base) that every build step of the userland,
# the runtime, the engine and the system-driver server runs in. Sourced by userland/config.sh, never
# executed; it reads DEBIAN_IMAGE, DEBIAN_SNAPSHOT and container_engine from there.
# shellcheck shell=bash

BASE_IMAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The tag: the first 16 hex digits of the SHA-256 of the image's inputs, so a change of one of them
# names a new image, and the parts that name the image build again.
base_image_tag() {
    (cat "$BASE_IMAGE_DIR/Containerfile.base" "$BASE_IMAGE_DIR/base-packages.lock" \
        "$BASE_IMAGE_DIR/../userland/packages.lock"
        echo "${DEBIAN_IMAGE:?}"
        echo "${DEBIAN_SNAPSHOT:?}") | sha256sum | cut -c1-16
}

base_image_name() {
    echo "ghcr.io/pocketward/pd2-build-base:$(base_image_tag)"
}

# Prints the base image's name once the container engine has it: the local image, else the image
# pulled from ghcr.io, else an image built here from Containerfile.base (build context android/)
# with that name. It never pushes (the release workflow does). The label links the image that the
# workflow pushes to the public repository on ghcr.io; it is not in the tag's inputs.
ensure_base_image() {
    local engine image
    engine="$(container_engine)"
    image="$(base_image_name)"
    if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
        if "$engine" pull --platform linux/amd64 "$image" >&2 2>/dev/null; then
            echo "pulled $image" >&2
        else
            echo "building $image (no such image on the registry)" >&2
            "$engine" build --platform linux/amd64 --build-arg "DEBIAN_IMAGE=$DEBIAN_IMAGE" \
                --build-arg "DEBIAN_SNAPSHOT=$DEBIAN_SNAPSHOT" \
                --label org.opencontainers.image.source=https://github.com/Pocketward/pd2-launcher-android \
                -t "$image" -f "$BASE_IMAGE_DIR/Containerfile.base" "$BASE_IMAGE_DIR/.." >&2 || return 1
        fi
    fi
    echo "$image"
}
