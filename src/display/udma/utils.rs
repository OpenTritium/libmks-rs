//! Shared utilities for DMA-BUF operations
//!
//! This module provides common functions used across different display backends
//! for creating DMA-BUF file descriptors and building textures.
use crate::display::{pixman_4cc::FourCC, udma::UdmabufCreate};
use relm4::gtk::{
    cairo::{RectangleInt, Region},
    gdk::{DmabufTextureBuilder, Texture},
    glib,
};
use rustix::{
    fd::{AsFd, AsRawFd, OwnedFd, RawFd},
    fs::{Mode, OFlags, open},
    ioctl::ioctl,
    param::page_size,
};
use std::{
    hint::unlikely,
    io,
    num::{NonZeroU32, NonZeroU64},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Fetches the cached system's page size.
#[inline(always)]
pub fn fetch_page_size() -> usize {
    static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);
    let size = PAGE_SIZE.load(Ordering::Relaxed);
    if unlikely(size != 0) {
        return size;
    }
    let size = page_size();
    PAGE_SIZE.store(size, Ordering::Relaxed);
    size
}

/// DRM format modifier for linear (row-major) memory layout, indicating no tiling or compression.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Represents a rectangular damage region.
#[derive(Clone, Copy, Debug)]
pub struct Damage {
    pub x: u32,
    pub y: u32,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
}

/// Describes a single plane within a DMA-BUF.
///
/// # Safety constraint
/// The `fd` field must contain a valid, open file descriptor.
/// The caller retains ownership of the FD and must ensure it remains open
/// for the duration of the texture creation.
#[derive(Clone, Copy, Debug)]
pub struct DmabufPlane {
    pub fd: RawFd,
    pub stride: NonZeroU32,
    pub offset: u32,
}

/// Creates a DMA-BUF file descriptor from a memfd region via `/dev/udmabuf`.
///
/// # Arguments
/// * `memfd`: The source memory file descriptor (must implement `AsFd`).
/// * `offset`: Start offset in bytes (must be page-aligned).
/// * `size`: Size in bytes (must be page-aligned).
///
/// # Errors
/// Returns `InvalidInput` if offset or size are not page-aligned.
/// Returns OS errors if `/dev/udmabuf` cannot be opened or ioctl fails.
#[inline]
pub fn create_udmabuf_fd(memfd: &impl AsFd, offset: u64, size: NonZeroU64) -> io::Result<OwnedFd> {
    let page_size = fetch_page_size() as u64;
    let size_not_aligned = !offset.is_multiple_of(page_size) || !size.get().is_multiple_of(page_size);
    if unlikely(size_not_aligned) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("udmabuf: offset and size must be page-aligned {page_size}"),
        ));
    }
    let udmabuf_dev = open("/dev/udmabuf", OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())?;
    let create_req = UdmabufCreate::new(memfd.as_fd().as_raw_fd(), offset, size);
    unsafe { ioctl(&udmabuf_dev, create_req) }.map_err(io::Error::from)
}

/// Builds a `GdkTexture` from multi-plane DMA-BUF data.
///
/// # Safety
/// * Caller guarantees all `planes` contain valid FDs.
/// * FDs must remain open for at least as long as the returned texture. If you need callback-based release, use GDK's
///   `build_with_release_func`.
#[inline]
pub fn build_dmabuf_texture_planar(
    width: NonZeroU32, height: NonZeroU32, fourcc: FourCC, modifier: u64, planes: &[DmabufPlane],
    update_texture: Option<&Texture>, damage: Option<Damage>,
) -> Result<Texture, glib::Error> {
    let num_planes = planes.len() as u32;
    let mut builder = DmabufTextureBuilder::new()
        .set_width(width.get())
        .set_height(height.get())
        .set_fourcc(fourcc.into())
        .set_modifier(modifier)
        .set_n_planes(num_planes)
        .set_update_texture(update_texture);
    if let Some(Damage { x, y, width, height }) = damage {
        let rect = RectangleInt::new(
            x.try_into().unwrap(),
            y.try_into().unwrap(),
            width.get().try_into().unwrap(),
            height.get().try_into().unwrap(),
        );
        let region = Region::create_rectangle(&rect);
        builder = builder.set_update_region(Some(&region));
    }
    let builder = planes.iter().enumerate().fold(builder, |b, (i, plane)| {
        let i = i as u32;
        let b = b.set_stride(i, plane.stride.get()).set_offset(i, plane.offset);
        unsafe { b.set_fd(i, plane.fd) }
    });
    unsafe { builder.build() }
}

#[cfg(test)]
mod tests {
    use crate::display::pixman_4cc::drm_4cc::ARGB8888;

    use super::*;
    use relm4::gtk::prelude::TextureExt;
    use rustix::{
        fs::{MemfdFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create},
        io::Errno,
    };
    use std::sync::Once;

    // 静态初始化 GTK，确保在测试进程中只初始化一次
    static GTK_INIT: Once = Once::new();

    fn try_init_gtk() -> bool {
        let mut initialized = false;
        GTK_INIT.call_once(|| {
            // 在 headless 环境下这也可能会失败，需要捕获
            initialized = relm4::gtk::init().is_ok();
        });
        // 这里的逻辑简化处理：如果已经初始化过或者本次初始化成功，认为可用
        // 注意：gtk::init() 在同一进程多次调用是安全的，但这里为了逻辑清晰用 Once
        relm4::gtk::is_initialized()
    }

    /// 辅助函数：创建一个真实且密封的 memfd
    /// 这是测试 create_udmabuf_fd 的前置条件
    fn create_valid_memfd(size: u64) -> OwnedFd {
        let name = std::ffi::CString::new("test_dmabuf").unwrap();
        let fd =
            memfd_create(&name, MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC).expect("当前内核不支持 memfd_create");

        ftruncate(&fd, size).expect("无法设置 memfd 大小");

        // udmabuf 通常要求 memfd 是 sealed 的（特别是 F_SEAL_SHRINK）
        fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW).expect("无法 seal memfd");

        fd
    }

    #[test]
    fn test_create_udmabuf_fd_alignment_check() {
        let size = NonZeroU64::new(4096).unwrap();
        let memfd = create_valid_memfd(size.get());

        match create_udmabuf_fd(&memfd, 123, size) {
            Ok(_) => panic!("Should fail due to misalignment"),
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "Should catch alignment error in userspace");
            }
        }
    }

    #[test]
    fn test_create_udmabuf_fd_driver_integration() {
        let size = NonZeroU64::new(4096).unwrap();
        let memfd = create_valid_memfd(size.get());

        match create_udmabuf_fd(&memfd, 0, size) {
            Ok(fd) => assert!(fd.as_raw_fd() > 0),
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidInput {
                    panic!("Valid arguments were rejected: {:?}", e);
                }
                match e.raw_os_error().map(Errno::from_raw_os_error) {
                    Some(Errno::NOENT) | Some(Errno::ACCESS) | Some(Errno::PERM) => {
                        println!("Skipping integration test: /dev/udmabuf not available/accessible");
                    }
                    _ => panic!("Unexpected kernel error: {:?}", e),
                }
            }
        }
    }

    #[test]
    fn test_build_dmabuf_texture_planar_logic() {
        // 这个测试尝试构建 Texture。
        // 注意：GDK Texture 构建通常需要连接到 Display Server (Wayland/X11)。
        // 如果在 CI 的 Headless 环境运行，GTK init 可能会失败。

        if !try_init_gtk() {
            println!("test_build_dmabuf_texture: 跳过 (无法初始化 GTK，可能是 headless 环境)");
            return;
        }

        // 模拟一个伪造的 fd (注意：这在 build() 时可能会被 GDK 校验并报错，但我们要测的是 builder 的调用链不 panic)
        // 在 Linux 上，我们可以用 stdout 的 fd 来凑数，或者再创建一个 memfd
        let fake_memfd = create_valid_memfd(4096);
        let fake_fd = fake_memfd.as_raw_fd();

        let planes = [DmabufPlane { fd: fake_fd, stride: NonZeroU32::new(100).unwrap(), offset: 1 }];

        // 尝试构建
        let result = build_dmabuf_texture_planar(
            100.try_into().unwrap(),
            100.try_into().unwrap(),
            ARGB8888, // 假设 FourCC 枚举里有这个
            DRM_FORMAT_MOD_LINEAR,
            &planes,
            None,
            None,
        );

        match result {
            Ok(texture) => {
                assert_eq!(texture.width(), 100);
                assert_eq!(texture.height(), 100);
            }
            Err(e) => {
                // GDK 可能会因为 FD 不是真正的 DMA-BUF 而报错
                // 或者因为不支持该格式而报错。
                // 只要这里没有发生内存安全 panic (unsafe block 出错)，测试就算通过
                println!("GDK 构建纹理失败 (这是预期的，因为我们用了伪造的 FD): {}", e);
            }
        }
    }
}
