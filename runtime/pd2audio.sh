#!/usr/bin/env bash
# The game's sound output for Wine's winealsa.drv: our ALSA plugin (pd2-audio/pcm_pd2.c, the PCM
# type "pd2" that writes into the Android app's ring, pd2-audio/pd2_audio_ring.h), built against
# Debian's alsa-lib in the Wine build image, and that alsa-lib (libasound.so.2), which the App
# userland does not carry. build.sh puts both beside winealsa.so, which finds libasound there
# ($ORIGIN). The app writes the ALSA configuration that names the plugin (GameAudio.kt).
# It first builds the plugin and its test (pd2-audio/pcm_pd2_test.c) for the build machine and runs
# the test. Output: parts/pd2audio/libasound_module_pcm_pd2.so and parts/pd2audio/libasound.so.2.
#
# Usage: pd2audio.sh [--identity]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

identity() {
    part_identity "debian=$DEBIAN_IMAGE $DEBIAN_SNAPSHOT" "$here/pd2audio.sh" "$RUNTIME_DIR/containers/Containerfile.wine" \
        "$here/pd2-audio/pcm_pd2.c" "$here/pd2-audio/pd2_audio_ring.h" "$here/pd2-audio/pcm_pd2_test.c"
}

if [[ "${1:-}" == --identity ]]; then
    identity
    exit 0
fi

if ! in_container; then
    id="$(identity)"
    if part_ready pd2audio "$id"; then
        echo "pd2audio: reusing parts/pd2audio"
        exit 0
    fi
    rm -rf "$RUNTIME_PARTS/pd2audio"
    mkdir -p "$RUNTIME_PARTS/pd2audio"
    run_in "$(runtime_image wine)" pd2audio.sh
    part_done pd2audio "$id"
    exit 0
fi

out=/cache/parts/pd2audio
# The plugin's test (pd2-audio/pcm_pd2_test.c), on the build machine against its own alsa-lib.
test=/tmp/pd2audio-test
rm -rf "$test"
mkdir -p "$test/run"
gcc -shared -fPIC -DPIC -O2 -Wall -Wextra -Werror -std=gnu11 -I/src/pd2-audio \
    -o "$test/libasound_module_pcm_pd2.so" /src/pd2-audio/pcm_pd2.c -lasound
gcc -O2 -Wall -Wextra -Werror -std=gnu11 -I/src/pd2-audio -o "$test/pcm_pd2_test" /src/pd2-audio/pcm_pd2_test.c -lasound
"$test/pcm_pd2_test" "$test/libasound_module_pcm_pd2.so" "$test/run"
rm -rf "$test"

aarch64-linux-gnu-gcc -shared -fPIC -DPIC -O2 -Wall -Wextra -Werror -std=gnu11 -fvisibility=default \
    -I/src/pd2-audio -o "$out/libasound_module_pcm_pd2.so" /src/pd2-audio/pcm_pd2.c \
    -L/usr/lib/aarch64-linux-gnu -lasound
aarch64-linux-gnu-strip --strip-unneeded "$out/libasound_module_pcm_pd2.so"
cp -L /usr/lib/aarch64-linux-gnu/libasound.so.2 "$out/libasound.so.2"
aarch64-linux-gnu-strip --strip-unneeded "$out/libasound.so.2"
ls -l "$out"
