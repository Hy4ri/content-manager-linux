#!/usr/bin/env bash
# Assemble a portable x86_64 AppImage from a compiled cm-rs binary and CEF runtime.
# Usage: scripts/package-appimage.sh <tag> <binary-path> <cef-runtime-dir> <out-dir>
set -euo pipefail

tag="$1"
bin_path="$2"
cef_dir="$3"
out_dir="$4"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
name="cm-rs-${tag}-x86_64.AppImage"
work="$(mktemp -d "${TMPDIR:-/tmp}/cm-rs-appimage.XXXXXX")"
trap 'rm -rf "$work"' EXIT

appdir="$work/cm-rs.AppDir"
libdir="$appdir/usr/lib/cm-rs"
mkdir -p "$libdir" "$out_dir"

cp -L "$bin_path" "$libdir/cm-rs"
chmod 0755 "$libdir/cm-rs"

# Copy CEF runtime files (excluding libvulkan.so.1 so GPUI uses the host Vulkan loader).
for f in libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so vk_swiftshader_icd.json \
         chrome_100_percent.pak chrome_200_percent.pak resources.pak icudtl.dat \
         v8_context_snapshot.bin; do
  cp -L "$cef_dir/$f" "$libdir/$f"
done
cp -rL "$cef_dir/locales" "$libdir/locales"

# Bundle libxkbcommon + libxkbcommon-x11 so minimal Wayland distros don't miss them.
for lib in libxkbcommon.so.0 libxkbcommon-x11.so.0; do
  resolved="$(ldd "$bin_path" | awk -v l="$lib" '$1 == l && $3 != "" { print $3; exit }')"
  if [ -n "$resolved" ] && [ -f "$resolved" ]; then
    cp -L "$resolved" "$libdir/$lib"
  fi
done

chmod -R u+w "$libdir"
patchelf --set-rpath '$ORIGIN' "$libdir/cm-rs"

# Desktop entry and icons
cat > "$appdir/cm-rs.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Content Manager (Rust)
GenericName=Assetto Corsa Launcher
Comment=Native launcher for Assetto Corsa: Quick Drive, cars, tracks, mod install
Exec=cm-rs
Icon=cm-rs
Terminal=false
Categories=Game;
Keywords=Assetto Corsa;AC;Racing;Content Manager;Proton;
StartupWMClass=cm-rs
EOF

cp "$repo_root/packaging/icons/cm-rs-256.png" "$appdir/cm-rs.png"
ln -sf cm-rs.png "$appdir/.DirIcon"
for s in 48 64 128 256 512; do
  install -Dm644 "$repo_root/packaging/icons/cm-rs-${s}.png" \
    "$appdir/usr/share/icons/hicolor/${s}x${s}/apps/cm-rs.png"
done

# AppRun launcher
cat > "$appdir/AppRun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(dirname "$(readlink -f "$0")")"
export CM_RS_CEF_PATH="$HERE/usr/lib/cm-rs"
export LD_LIBRARY_PATH="$HERE/usr/lib/cm-rs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/lib/cm-rs/cm-rs" "$@"
EOF
chmod 0755 "$appdir/AppRun"

ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "${APPIMAGETOOL:-appimagetool}" \
  --no-appstream "$appdir" "$out_dir/$name"
chmod 0755 "$out_dir/$name"
echo "wrote $out_dir/$name"
