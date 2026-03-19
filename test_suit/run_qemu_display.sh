#!/bin/bash

# 配置
ISO="livecd.fedora.iso"
RAM="8G"
CPU="8"

# 默认设置
GPU="virtgpu"
NET="user"
MOUSE="relative"

# 清理进程
cleanup() {
    [ -n "$QEMU_PID" ] && kill -TERM "$QEMU_PID" 2>/dev/null
    [ -n "$PASST_PID" ] && kill -TERM "$PASST_PID" 2>/dev/null
    rm -f /tmp/vm_net_$$.socket
}
trap cleanup EXIT INT TERM

# 参数解析
while [[ $# -gt 0 ]]; do
    case $1 in
        -g|--gpu) GPU="$2"; shift 2 ;;
        -n|--net) NET="$2"; shift 2 ;;
        -m|--mouse) MOUSE="$2"; shift 2 ;;
        *) exit 1 ;;
    esac
done

# 构建参数组
ARGS=(
    -enable-kvm -smp "$CPU" -m "$RAM"
    -name "Fedora-VM" -uuid "12345678-1234-5678-1234-567812345678"
    -machine "q35,memory-backend=mem"
    -object "memory-backend-memfd,id=mem,size=$RAM,share=on"
    -cdrom "$ISO"
    -no-reboot
    -device "virtio-keyboard-pci"
)

# GPU 逻辑
case "$GPU" in
    vga)
        ARGS+=("-display" "dbus" "-device" "virtio-vga")
        ;;
    virtgpu)
        ARGS+=("-display" "dbus,gl=on" "-device" "virtio-vga-gl")
        ;;
    virtgpu-vulkan)
        ARGS+=("-display" "dbus,gl=on" "-device" "virtio-vga-gl,blob=true,hostmem=4G,venus=on")
        ;;
esac

# 网络逻辑
if [[ "$NET" == "passt-vhost" ]]; then
    SOCKET_PATH="/tmp/vm_net_$$.socket"
    passt --vhost-user -1 -t none -u none -s "$SOCKET_PATH" & PASST_PID=$!
    # 等待 Unix socket 就绪，避免 QEMU 在 socket 尚未创建时启动失败。
    for _ in {1..50}; do
        if [[ -S "$SOCKET_PATH" ]]; then
            break
        fi
        sleep 0.05
    done
    if [[ ! -S "$SOCKET_PATH" ]]; then
        echo "passt socket not available at $SOCKET_PATH; aborting" >&2
        exit 1
    fi
    ARGS+=(
        "-chardev" "socket,id=net0,path=$SOCKET_PATH"
        "-netdev" "vhost-user,id=net0,chardev=net0"
        "-device" "virtio-net-pci,netdev=net0"
    )
else
    ARGS+=("-netdev" "user,id=net0" "-device" "virtio-net-pci,netdev=net0")
fi

# 鼠标逻辑
ARGS+=("-device" "$([[ "$MOUSE" == "absolute" ]] && echo "virtio-tablet-pci" || echo "virtio-mouse-pci")")

# 打印命令（使用 printf 将数组优雅地展示出来）
echo -e "\033[0;32mStarting QEMU command:\033[0m"
printf "%s " qemu-system-x86_64 "${ARGS[@]}"
echo -e "\n"

# 启动
qemu-system-x86_64 "${ARGS[@]}" &
QEMU_PID=$!

wait "$QEMU_PID"
