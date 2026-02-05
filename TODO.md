# 1. 启动 vhost-user-gpu 守护进程
/usr/lib/qemu/vhost-user-gpu --socket-path=/tmp/gpu.sock &

# 2. 启动 QEMU 并引用该 Socket
qemu-system-x86_64 \
  -chardev socket,id=vug,path=/tmp/gpu.sock \
  -device vhost-user-gpu-pci,chardev=vug \
  -display dbus,gl=on