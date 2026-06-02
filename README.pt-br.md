# Android Webcam

[English](README.md) | Português (Brasil)

Use seu dispositivo Android como uma webcam de alta qualidade no Linux via `scrcpy` e `v4l2loopback`.

## 🛠️ Pré-requisitos

Antes de instalar ou compilar, certifique-se de que seu sistema possui os drivers e ferramentas necessários:

### **Para Debian / Ubuntu / Mint:**

```bash
sudo apt update
sudo apt install v4l2loopback-dkms v4l2loopback-utils adb ffmpeg libsdl2-2.0-0

```

#### Instalar scrcpy (Debian 13+)

```bash
git clone https://github.com/Genymobile/scrcpy
cd scrcpy
./install_release.sh
cd ..

```

### **Para Arch Linux:**

```bash
# Instalar dependências principais
sudo pacman -S v4l2loopback-dkms v4l2loopback-utils android-tools ffmpeg sdl2 scrcpy dkms

# Instalar headers do kernel (devem COINCIDIR com seu kernel atual)
sudo pacman -S linux-headers

# Carregar o módulo para a sessão atual
sudo modprobe v4l2loopback

```

---

## 📥 Instalação

### **Para Debian:**

```bash
sudo apt install ./android-webcam_*_amd64.deb

```

### **Para Arch Linux:**

Use o script de build do Arch fornecido abaixo para gerar e instalar o pacote automaticamente.

---

## 🔧 Configuração Pós-Instalação

### **1. Fixar no Kernel (Carregamento Persistente)**

**Debian / Ubuntu:**

```bash
sudo update-initramfs -u

```

**Arch Linux:**

```bash
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
echo "options v4l2loopback devices=1 video_nr=9 card_label='Android-Webcam' exclusive_caps=1" | sudo tee /etc/modprobe.d/v4l2loopback.conf
sudo mkinitcpio -P

```

### **2. Permissões de Usuário**

```bash
sudo usermod -aG video $USER

```

*(Nota: Reinicie a sessão para que as alterações entrem em vigor.)*

---

## 🚀 Compilação e Testes

Scripts de build padronizados são fornecidos para cada distribuição. Execute aquele que corresponde ao seu sistema operacional:

### **1. Tornar os scripts executáveis**

```bash
chmod +x build-debian.sh build-arch.sh

```

### **2. Executar o Script de Build**

**Se você estiver no Debian / Ubuntu / Mint:**

```bash
./build-debian.sh

```

**Se você estiver no Arch Linux / EndeavourOS / Manjaro:**

```bash
./build-arch.sh

```

---

## 📱 Uso

1. **Conectar**: Conecte seu telefone via USB e ative a **Depuração USB**.
2. **Autorizar**: Toque em **Permitir** na confirmação de depuração USB no seu telefone.
3. **Iniciar**: Abra o **Android Webcam** pelo seu Menu de Aplicativos ou Terminal.
4. **Configurar**: Selecione a câmera, resolução e FPS, então clique em **Launch**.
5. **Selecionar Fonte**: No seu aplicativo (Zoom, OBS, Discord), escolha **"Android-Webcam"**.

---

## ❤️ Créditos

Este projeto é baseado no incrível trabalho do projeto **scrcpy**.

* **scrcpy GitHub**: [https://github.com/Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)
