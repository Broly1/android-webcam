# Android Webcam

[Português (Brasil)](README.pt-br.md) | English

Use your Android device as a high-quality webcam on Linux via `scrcpy` and `v4l2loopback`.

## 🛠️ Prerequisites

Before installing or building, ensure your system has the necessary drivers and tools:

### **For Debian / Ubuntu / Mint:**

```bash
sudo apt update
sudo apt install v4l2loopback-dkms v4l2loopback-utils adb ffmpeg libsdl2-2.0-0

```

#### Install scrcpy (Debian 13+)

Since `scrcpy` may not be in the default repositories for Debian 13 yet, build it from the official source:

```bash
git clone https://github.com/Genymobile/scrcpy
cd scrcpy
./install_release.sh
cd ..

```

### **For Arch Linux:**

On Arch, you need `dkms` and the specific headers for your kernel to ensure the module builds correctly.

```bash
# Install core dependencies
sudo pacman -S v4l2loopback-dkms v4l2loopback-utils android-tools ffmpeg sdl2 scrcpy dkms

# Install kernel headers (MATCH these to your running kernel)
# If using the standard kernel:
sudo pacman -S linux-headers
# If using the LTS kernel:
# sudo pacman -S linux-lts-headers

# Load the module for the current session
sudo modprobe v4l2loopback

```

---

## 📥 Installation

### **For Debian:**

If you have downloaded the pre-compiled `.deb` package, you can install it directly:

```bash
sudo apt install ./android-webcam_1.0.0-1_amd64.deb

```

### **For Arch Linux:**

Arch users should use the build script provided below to generate and install the package via `makepkg`.

---

## 🔧 Post-Install Setup

To ensure the webcam is available every time you restart and works **without a password prompt**, you **must** run these once:

### **1. Bake into Kernel (Persistent Loading)**

Ensures the `v4l2loopback` module loads at boot.

**Debian / Ubuntu:**

```bash
sudo update-initramfs -u

```

**Arch Linux:**
Arch requires explicit configuration to load the module and set the correct device parameters at boot.

```bash
# 1. Force the module to load at boot
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf

# 2. Configure the device (Name, ID, and Exclusive Caps)
echo "options v4l2loopback devices=1 video_nr=128 card_label='Android-Webcam' exclusive_caps=1" | sudo tee /etc/modprobe.d/v4l2loopback.conf

# 3. Regenerate boot image
sudo mkinitcpio -P

```

### **2. User Permissions**

Adds your user to the `video` group to access the camera device without `sudo`.

```bash
sudo usermod -aG video $USER

```

*(Note: You must log out and back in for this to take effect.)*

---

## 🚀 Building and Testing

The included interactive `build.sh` script handles compilation and package creation for your specific distribution.

1. **Make the script executable**:

```bash
chmod +x build.sh

```

2. **Run the Build System**:

```bash
./build.sh

```

---

## 📱 Usage

1. **Connect**: Plug in your phone via USB and enable **USB Debugging** in Developer Options.
2. **Authorize**: Check your phone screen and tap **Allow** for the USB debugging prompt.
3. **Launch**: Open **Android Webcam** from your App Menu or Terminal.
4. **Configure**: Select your camera (front/back), resolution, and FPS, then click **Launch**.
5. **Select Source**: In Zoom, OBS, or Discord, choose **"Android-Webcam"** as your camera.

---

## ❤️ Credits

This project is built upon the incredible work of the **scrcpy** project.

* **scrcpy GitHub**: [https://github.com/Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)
