# Android Webcam

Use seu dispositivo Android como uma webcam de alta qualidade no Linux via `scrcpy` e `v4l2loopback`.

## 🛠️ Pré-requisitos

Antes de instalar ou compilar, certifique-se de que seu sistema possui os drivers e ferramentas necessários:

### **Para Debian / Ubuntu / Mint:**

```bash
sudo apt update
sudo apt install v4l2loopback-dkms v4l2loopback-utils adb ffmpeg libsdl2-2.0-0

```

#### Instalar scrcpy (Debian 13+)

Como o `scrcpy` pode não estar nos repositórios padrão do Debian 13 ainda, compile-o da fonte oficial:

```bash
git clone https://github.com/Genymobile/scrcpy
cd scrcpy
./install_release.sh
cd ..

```

### **Para Arch Linux:**

No Arch, você precisa do `dkms` e dos headers específicos para o seu kernel para garantir que o módulo seja compilado corretamente.

```bash
# Instalar dependências principais
sudo pacman -S v4l2loopback-dkms v4l2loopback-utils android-tools ffmpeg sdl2 scrcpy dkms

# Instalar headers do kernel (DEVEM CORRESPONDER ao seu kernel em execução)
# Se estiver usando o kernel padrão:
sudo pacman -S linux-headers
# Se estiver usando o kernel LTS:
# sudo pacman -S linux-lts-headers

# Carregar o módulo para a sessão atual
sudo modprobe v4l2loopback

```

---

## 📥 Instalação

### **Para Debian:**

Se você baixou o pacote `.deb` pré-compilado, pode instalá-lo diretamente:

```bash
sudo apt install ./android-webcam_1.0.0-1_amd64.deb

```

### **Para Arch Linux:**

Usuários do Arch devem usar o script de compilação fornecido abaixo para gerar e instalar o pacote via `makepkg`.

---

## 🔧 Configuração Pós-Instalação

Para garantir que a webcam esteja disponível sempre que você reiniciar e funcione **sem solicitar senha**, você **deve** executar estes comandos uma vez:

### **1. Fixar no Kernel (Carregamento Persistente)**

Garante que o módulo `v4l2loopback` seja carregado na inicialização.

**Debian / Ubuntu:**

```bash
sudo update-initramfs -u

```

**Arch Linux:**
O Arch requer configuração explícita para carregar o módulo e definir os parâmetros corretos do dispositivo no boot.

```bash
# 1. Forçar o carregamento do módulo no boot
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf

# 2. Configurar o dispositivo (Nome, ID e Exclusive Caps)
echo "options v4l2loopback devices=1 video_nr=128 card_label='Android-Webcam' exclusive_caps=1" | sudo tee /etc/modprobe.d/v4l2loopback.conf

# 3. Regenerar a imagem de boot
sudo mkinitcpio -P

```

### **2. Permissões de Usuário**

Adiciona seu usuário ao grupo `video` para acessar o dispositivo de câmera sem necessidade de `sudo`.

```bash
sudo usermod -aG video $USER

```

*(Nota: Você deve encerrar a sessão e fazer login novamente para que isso surta efeito.)*

---

## 🚀 Compilação e Teste

O script interativo `build.sh` incluído gerencia a compilação e a criação do pacote para sua distribuição específica.

1. **Tornar o script executável**:

```bash
chmod +x build.sh

```

2. **Executar o Sistema de Build**:

```bash
./build.sh

```

---

## 📱 Uso

1. **Conectar**: Conecte seu telefone via USB e ative a **Depuração USB** nas Opções do Desenvolvedor.
2. **Autorizar**: Verifique a tela do seu telefone e toque em **Permitir** para a solicitação de depuração USB.
3. **Iniciar**: Abra o **Android Webcam** no menu de aplicativos ou terminal.
4. **Configurar**: Selecione sua câmera (frontal/traseira), resolução e FPS, então clique em **Launch**.
5. **Selecionar Fonte**: No Zoom, OBS ou Discord, escolha **"Android-Webcam"** como sua câmera.

---

## ❤️ Créditos

Este projeto é baseado no incrível trabalho do projeto **scrcpy**.

* **scrcpy GitHub**: [https://github.com/Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)
