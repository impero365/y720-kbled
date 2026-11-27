#!/bin/bash
# Build a Ubuntu 24.04 .deb from the release binary. Run it on the machine
# that will use the package, or on another amd64 Linux with the same glibc.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
version=1.1.0
# Raised on every build, like pkgrel in the PKGBUILD. Back to 1 on a new version.
release=1
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

cargo build --release --locked --manifest-path "$root/Cargo.toml"

mkdir -p \
  "$stage/DEBIAN" \
  "$stage/usr/bin" \
  "$stage/usr/lib/udev/rules.d" \
  "$stage/usr/lib/sysusers.d" \
  "$stage/usr/lib/systemd/system" \
  "$stage/etc/sudoers.d" \
  "$stage/usr/share/applications" \
  "$stage/usr/share/icons/hicolor/128x128/apps"

install -m755 "$root/target/release/y720-kbled" "$stage/usr/bin/y720-kbled"
install -m644 "$root/packaging/70-y720-kbled.rules" "$stage/usr/lib/udev/rules.d/70-y720-kbled.rules"
install -m644 "$root/packaging/y720-kbled.sysusers" "$stage/usr/lib/sysusers.d/y720-kbled.conf"
install -m644 "$root/packaging/y720-kbled.service" "$stage/usr/lib/systemd/system/y720-kbled.service"
install -m440 "$root/packaging/y720-kbled.sudoers" "$stage/etc/sudoers.d/y720-kbled"
install -m644 "$root/packaging/y720-kbled.desktop" "$stage/usr/share/applications/y720-kbled.desktop"
install -m644 "$root/assets/128x128.png" "$stage/usr/share/icons/hicolor/128x128/apps/y720-kbled.png"

cat > "$stage/DEBIAN/control" << EOF
Package: y720-kbled
Version: $version-$release
Section: utils
Priority: optional
Architecture: amd64
Depends: libgtk-4-1, sudo, systemd
Maintainer: Lenovo Y720 Keyboard Backlight <y720-kbled@localhost>
Description: Keyboard backlight controller for the Lenovo Legion Y720
 Sets colors, brightness, and effects for the ITE33D1 keyboard backlight
 and switches profiles with Fn+Space.
EOF

cat > "$stage/DEBIAN/postinst" << 'EOF'
#!/bin/sh
set -e
if command -v systemd-sysusers >/dev/null 2>&1; then
  systemd-sysusers || true
fi
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=hidraw --subsystem-match=input || true
fi
systemctl daemon-reload || true
systemctl enable --now y720-kbled.service || true
EOF
chmod 755 "$stage/DEBIAN/postinst"

cat > "$stage/DEBIAN/prerm" << 'EOF'
#!/bin/sh
set -e
systemctl disable --now y720-kbled.service || true
EOF
chmod 755 "$stage/DEBIAN/prerm"

cat > "$stage/DEBIAN/postrm" << 'EOF'
#!/bin/sh
set -e
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=hidraw --subsystem-match=input || true
fi
systemctl daemon-reload || true
rm -rf /run/y720-kbled
if [ "$1" = "purge" ]; then
  rm -rf /var/lib/y720-kbled
  if id y720 >/dev/null 2>&1; then
    userdel y720 || true
  fi
fi
EOF
chmod 755 "$stage/DEBIAN/postrm"

out="$root/packaging/y720-kbled_${version}-${release}_amd64.deb"
dpkg-deb --root-owner-group --build "$stage" "$out"
echo "wrote $out"
