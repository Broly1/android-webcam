# Android Webcam

[Portugues (Brasil)](README.pt-br.md) | English

Use your Android device as a high-quality webcam on Linux via `scrcpy` and `v4l2loopback`.

## 🛠️ Prerequisites

Before installing or building, ensure your system has the necessary drivers and tools:

### **For Debian / Ubuntu / Mint:**

```bash
sudo apt update
sudo apt install v4l2loopback-dkms v4l2loopback-utils adb ffmpeg libsdl2-2.0-0

```

#### Install scrcpy (Debian 13+)

```bash
git clone https://github.com/Genymobile/scrcpy
cd scrcpy
./install_release.sh
cd ..

```

### **For Arch Linux:**

```bash
# Install core dependencies
sudo pacman -S v4l2loopback-dkms v4l2loopback-utils android-tools ffmpeg sdl2 scrcpy dkms

# Install kernel headers (MATCH these to your running kernel)
sudo pacman -S linux-headers

# Load the module for the current session
sudo modprobe v4l2loopback

```

---

## 📥 Installation

### **For Debian:**

```bash
sudo apt install ./android-webcam_*_amd64.deb

```

### **For Arch Linux:**

Use the provided Arch build script to generate and install the package automatically.

---

## 🔧 Post-Install Setup

### **1. Bake into Kernel (Persistent Loading)**

**Debian / Ubuntu:**

```bash
sudo update-initramfs -u

```

**Arch Linux:**

```bash
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
echo "options v4l2loopback devices=1 video_nr=128 card_label='Android-Webcam' exclusive_caps=1" | sudo tee /etc/modprobe.d/v4l2loopback.conf
sudo mkinitcpio -P

```

### **2. User Permissions**

```bash
sudo usermod -aG video $USER

```

*(Note: Log out and back in for changes to take effect.)*

---

## 🚀 Building and Testing

Standardized build scripts are provided for each distribution. Run the one that matches your OS:

### **1. Make scripts executable**

```bash
chmod +x build-debian.sh build-arch.sh

```

### **2. Run the Build Script**

**If you are on Debian / Ubuntu / Mint:**

```bash
./build-debian.sh

```

**If you are on Arch Linux / EndeavourOS / Manjaro:**

```bash
./build-arch.sh

```

---

## 📱 Usage

1. **Connect**: Plug in your phone via USB and enable **USB Debugging**.
2. **Authorize**: Tap **Allow** for the USB debugging prompt on your phone.
3. **Launch**: Open **Android Webcam** from your App Menu or Terminal.
4. **Configure**: Select camera, resolution, and FPS, then click **Launch**.
5. **Select Source**: In Zoom, OBS, or Discord, choose **"Android-Webcam"**.

---

## ❤️ Credits

This project is built upon the incredible work of the **scrcpy** project.

* **scrcpy GitHub**: [https://github.com/Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)
