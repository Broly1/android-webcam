#!/bin/bash

clear
echo "=========================================="
echo "      Android Webcam Build System         "
echo "=========================================="

VERSION=$(grep '^version' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
GIT_HASH=$(git rev-parse --short HEAD 2>/dev/null || echo "dev")

echo "Current Version: $VERSION ($GIT_HASH)"
echo "------------------------------------------"

read -p "Perform a CLEAN build? (y/N): " clean_choice

if [[ "$clean_choice" =~ ^[yY]$ ]]; then
    echo "🧹 Cleaning previous build artifacts..."
    cargo clean
fi

cargo update

echo "🚀 Building for Debian..."
cargo build --release
echo "📦 Generating .deb package..."
cargo deb

DEB_FILE=$(ls target/debian/android-webcam_${VERSION}*.deb 2>/dev/null | head -n 1)
if [ -f "$DEB_FILE" ]; then
    echo "✅ Success: $DEB_FILE"
    read -p "Install and test this version? (y/N): " install_choice
    if [[ "$install_choice" =~ ^[yY]$ ]]; then
        sudo dpkg -i --force-depends "$DEB_FILE"
        sudo update-initramfs -u
        echo "✨ Done!"
    fi
else
    echo "❌ Error: .deb not found."
fi