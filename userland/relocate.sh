#!/usr/bin/env bash
# Makes the staging tree runnable at the prefix $USERLAND (config.sh, below the placeholder data
# folder the app rewrites to its own), in place:
#   - drops documentation (all of usr/share/doc) and translations
#   - absolute symlinks become relative; hard links become separate files (SELinux forbids
#     hard links in app storage); setuid/setgid bits are cleared
#   - ensures dri/zink_dri.so (Zink's DRI driver name; a link to libdril_dri.so where Mesa's
#     install left none)
#   - every ELF's PT_INTERP becomes $USERLAND/lib/ld-linux-aarch64.so.1; absolute RUNPATH/RPATH
#     entries get the $USERLAND prefix (patchelf, pinned in the tools image)
#   - writes etc/: fonts.conf, resolv.conf placeholder, nsswitch.conf, hosts, the CA bundle and
#     userland.env (the runtime knobs, from userland.env.in; without its LD_LIBRARY_PATH line when
#     PD2_USERLAND_WITH_GLIBC=1, since our loader searches the tree itself)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$here/config.sh"

if [[ "${PD2_USERLAND_IN_CONTAINER:-}" != 1 ]]; then
    RUN_EXTRA=(-e "PD2_USERLAND_WITH_GLIBC=${PD2_USERLAND_WITH_GLIBC:-0}")
    run_in_tools relocate.sh "$@"
    exit
fi

root=/cache/work/staging
[[ -d "$root/usr" ]] || { echo "no staging tree: run fetch-debian.sh first" >&2; exit 1; }
interp="$USERLAND/lib/ld-linux-aarch64.so.1"

# --- prune -----------------------------------------------------------------------------------
rm -rf "$root"/usr/share/{man,info,lintian,bug,doc,doc-base,locale,gtk-doc,bash-completion,zsh,menu}
rm -rf "$root"/etc/init.d "$root"/usr/lib/systemd "$root"/usr/lib/udev

# --- links and modes -----------------------------------------------------------------------
relativize_symlinks "$root"
[[ -e "$root/usr/lib/aarch64-linux-gnu/dri/zink_dri.so" ]] ||
    ln -sfn libdril_dri.so "$root/usr/lib/aarch64-linux-gnu/dri/zink_dri.so"
while IFS= read -r -d '' f; do
    cp -p "$f" "$f.unlink" && mv -f "$f.unlink" "$f"
done < <(find "$root" -type f -links +1 -print0)
find "$root" -type f -perm /6000 -exec chmod ug-s {} +

# --- ELF interpreter and run paths ---------------------------------------------------------
patched=0; rpaths=0
while IFS= read -r -d '' f; do
    [[ "$(head -c4 "$f" | od -An -tx1 | tr -d ' \n')" == 7f454c46 ]] || continue
    if old="$(patchelf --print-interpreter "$f" 2>/dev/null)"; then
        if [[ "$old" != "$interp" ]]; then
            patchelf --set-interpreter "$interp" "$f"
            patched=$((patched + 1))
        fi
    fi
    rp="$(patchelf --print-rpath "$f" 2>/dev/null || true)"
    if [[ -n "$rp" ]]; then
        new=""
        IFS=: read -ra parts <<<"$rp"
        for p in "${parts[@]}"; do
            [[ "$p" == /* && "$p" != "$USERLAND"/* ]] && p="$USERLAND$p"
            new+="${new:+:}$p"
        done
        if [[ "$new" != "$rp" ]]; then
            # Keep the tag: DT_RPATH stays DT_RPATH (--force-rpath), DT_RUNPATH stays DT_RUNPATH.
            if aarch64-linux-gnu-readelf -d "$f" | grep -q '(RPATH)'; then
                patchelf --force-rpath --set-rpath "$new" "$f"
            else
                patchelf --set-rpath "$new" "$f"
            fi
            rpaths=$((rpaths + 1))
        fi
    fi
done < <(find "$root" -type f -print0)
echo "interpreters set: $patched; run paths prefixed: $rpaths"
[[ -e "$root/usr/lib/ld-linux-aarch64.so.1" ]] || { echo "no loader at lib/ld-linux-aarch64.so.1" >&2; exit 1; }

# --- etc -----------------------------------------------------------------------------------
mkdir -p "$root/etc/fonts" "$root/etc/ssl/certs"
cat > "$root/etc/fonts/fonts.conf" <<EOF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<!-- App userland font configuration (FONTCONFIG_FILE, see etc/userland.env). -->
<fontconfig>
  <dir>$USERLAND/usr/share/fonts</dir>
  <cachedir prefix="xdg">fontconfig</cachedir>
  <include ignore_missing="yes">$USERLAND/etc/fonts/conf.d</include>
</fontconfig>
EOF
cat > "$root/etc/resolv.conf" <<'EOF'
# Placeholder: the app overwrites this file with the device's DNS servers before starting programs.
# Read only with the glibc patch (glibc reads the hard-coded /etc/resolv.conf otherwise).
EOF
cat > "$root/etc/nsswitch.conf" <<'EOF'
# Read only with the glibc patch (glibc reads the hard-coded /etc/nsswitch.conf otherwise).
passwd: files
group:  files
shadow: files
hosts:  files dns
EOF
cat > "$root/etc/hosts" <<'EOF'
127.0.0.1 localhost
::1       localhost ip6-localhost ip6-loopback
EOF
# What update-ca-certificates would build (maintainer scripts are not run): every Mozilla CA.
find "$root/usr/share/ca-certificates/mozilla" -name '*.crt' -print0 | sort -z \
    | xargs -0 cat > "$root/etc/ssl/certs/ca-certificates.crt"
drop_ldpath=()
[[ "${PD2_USERLAND_WITH_GLIBC:-0}" == 1 ]] && drop_ldpath=(-e '/^LD_LIBRARY_PATH=/d')
sed -e "s|@USERLAND@|$USERLAND|g" -e "s|@DATA@|$DATA_PLACEHOLDER|g" "${drop_ldpath[@]}" \
    "$here/userland.env.in" > "$root/etc/userland.env"

echo "relocated: $(find "$root" -type f | wc -l) files, $(find "$root" -type l | wc -l) symlinks, $(du -sh "$root" | cut -f1)"
