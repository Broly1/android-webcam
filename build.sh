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

echo ""
echo "Which package type do you want to build?"
echo "1) Debian/Ubuntu (.deb)"
echo "2) Arch Linux (PKGBUILD)"
read -p "Selection (1 or 2): " dist_choice

if [[ "$clean_choice" =~ ^[yY]$ ]]; then
    echo "🧹 Cleaning previous build artifacts..."
    cargo clean
fi

cargo update

if [ "$dist_choice" == "1" ]; then
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

elif [ "$dist_choice" == "2" ]; then
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

else
    echo "❌ Invalid selection. Exiting."
    exit 1
fi