//! https://github.com/torvalds/linux/blob/master/include/uapi/linux/udmabuf.h
pub mod utils;
use rustix::{
    fd::{FromRawFd, OwnedFd, RawFd},
    io::Result,
    ioctl::{Ioctl, IoctlOutput, Opcode, opcode},
};
use std::ffi::c_void;
pub use utils::{DRM_FORMAT_MOD_LINEAR, Damage, DmabufPlane, build_dmabuf_texture_planar, create_udmabuf_fd};

const UDMABUF_FLAGS_CLOEXEC: u32 = 0x01;

#[repr(C)]
#[derive(Debug, Default)]
pub struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

impl UdmabufCreate {
    #[inline]
    pub const fn new(memfd: RawFd, offset: u64, size: u64) -> Self {
        Self { memfd: memfd as u32, flags: UDMABUF_FLAGS_CLOEXEC, offset, size }
    }
}

const UDMABUF_MAGIC: u8 = b'u';
const UDMABUF_CMD_CREATE: u8 = 0x42;

unsafe impl Ioctl for UdmabufCreate {
    type Output = OwnedFd;

    const IS_MUTATING: bool = false;

    #[inline]
    fn opcode(&self) -> Opcode { opcode::write::<UdmabufCreate>(UDMABUF_MAGIC, UDMABUF_CMD_CREATE) }

    #[inline]
    fn as_ptr(&mut self) -> *mut c_void { self as *mut Self as *mut c_void }

    #[inline]
    unsafe fn output_from_ptr(out: IoctlOutput, _ptr: *mut c_void) -> Result<Self::Output> {
        // SAFETY: UDMABUF_CREATE returns a new file descriptor on success.
        // We take ownership of this FD, so wrapping it in OwnedFd is safe.
        unsafe { Ok(OwnedFd::from_raw_fd(out as RawFd)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    /// 测试 1: 验证内存布局 (ABI Compatibility)
    /// 这是最重要的测试。必须确保 Rust 结构体和 C 结构体在内存中一模一样。
    /// struct udmabuf_create {
    ///     u32 memfd;  // 4 bytes
    ///     u32 flags;  // 4 bytes
    ///     u64 offset; // 8 bytes
    ///     u64 size;   // 8 bytes
    /// };
    /// 总大小应为 24 字节，对齐通常为 8 字节。
    #[test]
    fn test_udmabuf_create_layout() {
        assert_eq!(mem::size_of::<UdmabufCreate>(), 24, "Size of UdmabufCreate mismatch");
        assert_eq!(mem::align_of::<UdmabufCreate>(), 8, "Alignment of UdmabufCreate mismatch");

        // 检查字段偏移量 (可选，但更严谨)
        let dummy = UdmabufCreate::default();
        let base_ptr = &dummy as *const _ as usize;
        let flags_ptr = &dummy.flags as *const _ as usize;
        let offset_ptr = &dummy.offset as *const _ as usize;

        assert_eq!(flags_ptr - base_ptr, 4, "Offset of flags should be 4");
        assert_eq!(offset_ptr - base_ptr, 8, "Offset of offset should be 8");
    }

    /// 测试 2: 验证 Opcode 生成逻辑
    /// 我们不依赖内核，而是验证生成的 u32 数值是否符合预期。
    #[test]
    fn test_opcode_generation() {
        let req = UdmabufCreate::new(0, 0, 1024);
        let op = req.opcode();

        // 打印生成的 opcode 方便调试
        // Linux ioctl 编码通常结构如下 (可能随架构变化，以 x86_64 为例):
        // Dir(2bit) | Size(14bit) | Type(8bit) | Nr(8bit)
        println!("Generated Opcode: 0x{:08x}", op);

        // 验证基本特征：
        // 1. 低 8 位必须是命令号 (0x42)
        assert_eq!(op & 0xFF, UDMABUF_CMD_CREATE as u32, "Command number mismatch");

        // 2. 次低 8 位必须是 Magic Number ('u' = 0x75)
        assert_eq!((op >> 8) & 0xFF, UDMABUF_MAGIC as u32, "Magic number mismatch");

        // 3. 验证方向是 Write (依赖 rustix 内部实现，通常 Write bit 会被设置)
        // 注意：具体的 bit 位置依赖架构，这里只做简单存在性检查
        // 如果 op 为 0，说明没有任何位被设置，肯定不对
        assert!(op != 0, "Opcode should not be zero");
    }

    /// 测试 3: 模拟调用 (Integration-like Test)
    /// 尝试在一个普通文件 (如 /dev/null 或临时文件) 上调用这个 ioctl。
    /// 预期结果：应该返回错误 `ENOTTY` (Inappropriate ioctl for device)。
    /// 只要代码没 Panic，且能正确传递 syscall，就说明封装是成功的。
    #[test]
    fn test_ioctl_call_mechanism() {
        use std::{fs::File, os::fd::AsRawFd};

        // 打开一个随便什么文件描述符，只要它不是 udmabuf 设备
        let file = File::open("/dev/null").unwrap();
        let fd = unsafe { rustix::fd::BorrowedFd::borrow_raw(file.as_raw_fd()) };

        // 构造请求
        let req = UdmabufCreate::new(100, 0, 4096); // 这里的 memfd 100 是假的

        // 执行 ioctl
        // 因为 fd 指向 /dev/null，内核找不到对应的 ioctl handler，
        // 应该返回 ENOTTY (Error 25)。
        let result = unsafe { rustix::ioctl::ioctl(fd, req) };

        match result {
            Ok(_) => panic!("Should not succeed on /dev/null"),
            Err(e) => {
                // 验证我们是否成功触发了系统调用流程
                // ENOTTY 证明参数正确传递进去了，只是设备不对。
                assert_eq!(e, rustix::io::Errno::NOTTY, "Expected ENOTTY on wrong device");
            }
        }
    }

    #[test]
    fn test_happy_path_simulation() {
        use std::{
            fs::File,
            os::fd::{AsRawFd, IntoRawFd},
        };

        // 1. 准备阶段
        // 我们需要一个真实的、有效的 FD 来模拟内核返回的新 FD。
        // 如果用随便一个数字（比如 1234），测试结束时 OwnedFd 尝试关闭它会因为 EBADF 而报错（或者关掉不该关的文件）。
        // 所以我们打开 /dev/null 作为一个“替身”。
        let fake_kernel_file = File::open("/dev/null").unwrap();
        let fake_raw_fd = fake_kernel_file.as_raw_fd();

        // 关键点：我们要把这个 File 的所有权“忘掉”，防止它在这里被 drop 关闭。
        // 因为我们待会儿要把它交给 CreateUdmabuf 里的 OwnedFd 去管理。
        // 这就模拟了“内核产生了一个新 FD 并移交给你”的过程。
        let _ = fake_kernel_file.into_raw_fd();

        // 2. 准备你的 IOCTL 结构体
        let mut req = UdmabufCreate::new(0, 0, 4096);

        // 3. 【核心黑魔法】手动扮演 rustix
        // 我们不调用 ioctl()，而是直接调用 trait 方法 output_from_ptr()。
        // 假设内核系统调用成功了，返回了 fake_raw_fd。
        let result = unsafe {
            // rustix 内部就是这样做的：
            // 当 syscall 返回成功值 (fake_raw_fd) 时，调用这个回调
            UdmabufCreate::output_from_ptr(fake_raw_fd as IoctlOutput, req.as_ptr())
        };

        // 4. 验证结果
        assert!(result.is_ok(), "模拟的成功路径应该返回 Ok");

        let owned_fd = result.unwrap();

        // 验证我们拿到的 OwnedFd 确实包裹了我们模拟的那个 FD
        assert_eq!(owned_fd.as_raw_fd(), fake_raw_fd, "返回的 OwnedFd 应该包含模拟的原始 FD");

        println!("成功模拟！OwnedFd 现在的 raw fd 是: {}", owned_fd.as_raw_fd());

        // 5. 清理
        // 测试结束时，owned_fd 会自动 Drop 并关闭 fake_raw_fd，
        // 这正是我们想验证的生命周期管理。
    }
}
