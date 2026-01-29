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

echo "🚀 Syncing PKGBUILD version..."
if [ -d "arch-configs" ]; then
    sed -i "s/^pkgver=.*/pkgver=$VERSION/" arch-configs/PKGBUILD
    
    cd arch-configs
    echo "🚀 Running makepkg..."
    makepkg -ifc
    
    if [ $? -eq 0 ]; then
        echo "✅ Success! Arch package installed."
        rm -f *.pkg.tar.zst
        cd ..
        echo "✨ Done!"
    else
        echo "❌ Error: makepkg failed."
        cd ..
        exit 1
    fi
else
    echo "❌ Error: arch-configs directory not found."
fi