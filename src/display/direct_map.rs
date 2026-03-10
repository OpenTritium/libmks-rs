use super::{
    Error,
    pixman_4cc::{FourCC, Pixman},
    udma::{DRM_FORMAT_MOD_LINEAR, DmabufPlane, build_dmabuf_texture_planar, create_udmabuf_fd},
};
use relm4::gtk::gdk::Texture;
use rustix::fs::{Stat, fstat};
use std::{
    num::NonZeroU32,
    os::fd::{AsRawFd, OwnedFd, RawFd},
};

/// Represents a guest memory region mapped for host GPU access via DMA-BUF.
///
/// This struct holds the ownership of the guest `memfd` and the derived `dmabuf_fd`,
/// along with file identity (device ID and inode) for robust caching.
#[derive(Debug)]
pub struct DmabufImport {
    _guest_memfd: OwnedFd,
    dmabuf_fd: OwnedFd,
    dev: u64,
    ino: u64,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub stride: NonZeroU32,
    pub offset: u32,
    pub pixman: Pixman,
}

impl DmabufImport {
    /// Creates a new mapping, capturing file identity for caching.
    #[inline]
    pub fn new(
        memfd: OwnedFd, stat: Stat, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32,
        pixman: Pixman,
    ) -> Result<Self, Error> {
        // `/dev/udmabuf` expects the sub-view length, not the memfd end offset.
        let size = height.checked_mul(stride).unwrap().into();
        let dmabuf_fd = create_udmabuf_fd(&memfd, offset as u64, size)?;
        Ok(Self {
            _guest_memfd: memfd,
            dmabuf_fd,
            dev: stat.st_dev,
            ino: stat.st_ino,
            width,
            height,
            stride,
            offset,
            pixman,
        })
    }

    /// Checks if the provided file identity and metadata match this mapping.
    #[inline]
    pub fn matches(
        &self, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32, pixman: Pixman, stat: &Stat
    ) -> bool {
        self.offset == offset
            && self.width == width
            && self.height == height
            && self.stride == stride
            && self.pixman == pixman
            && self.dev == stat.st_dev
            && self.ino == stat.st_ino
    }

    #[inline]
    pub fn as_raw_dmabuf_fd(&self) -> RawFd { self.dmabuf_fd.as_raw_fd() }

    #[inline]
    fn plane(&self) -> DmabufPlane {
        DmabufPlane { fd: self.as_raw_dmabuf_fd(), stride: self.stride, offset: self.offset }
    }
}

#[derive(Debug)]
pub struct ImportedTexture {
    buffer: Option<DmabufImport>,
    texture: Option<Texture>,
}

impl ImportedTexture {
    #[inline]
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self { Self { buffer: None, texture: None } }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn rebuild_texture(
        &mut self, memfd: OwnedFd, stat: Stat, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32,
        pixman: Pixman,
    ) -> Result<(), Error> {
        let fourcc: FourCC = pixman.try_into()?;
        let buffer = DmabufImport::new(memfd, stat, offset, width, height, stride, pixman)?;
        let plane = buffer.plane();
        let texture = build_dmabuf_texture_planar(width, height, fourcc, DRM_FORMAT_MOD_LINEAR, &[plane], None, None)?;
        self.buffer = Some(buffer);
        self.texture = Some(texture);
        Ok(())
    }

    /// Updates the texture, reusing the existing mapping if the underlying memory object (inode) matches.
    #[inline]
    pub fn update_texture(
        &mut self, memfd: OwnedFd, offset: u32, width: NonZeroU32, height: NonZeroU32, stride: NonZeroU32,
        pixman_format: Pixman,
    ) -> Result<(), Error> {
        let stat = fstat(&memfd)?;
        if let Some(buf) = &self.buffer
            && buf.matches(offset, width, height, stride, pixman_format, &stat)
        {
            return Ok(());
        }
        self.rebuild_texture(memfd, stat, offset, width, height, stride, pixman_format)
    }

    #[inline]
    pub const fn texture(&self) -> Option<&Texture> { self.texture.as_ref() }

    /// Returns the current buffer resolution.
    ///
    /// - `width`: buffer width in pixels.
    /// - `height`: buffer height in pixels.
    #[inline]
    pub fn resolution(&self) -> (u32, u32) {
        self.buffer.as_ref().map(|b| (b.width.get(), b.height.get())).unwrap_or_default()
    }

    #[inline]
    pub fn clear(&mut self) {
        self.buffer = None;
        self.texture = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{MemfdFlags, memfd_create};

    /// 辅助函数：安全地创建一个用于测试的 Mock GuestMapping
    /// 避免使用 FD 1 (stdout) 导致测试输出被关闭
    fn create_mock_mapping(
        memfd: OwnedFd, offset: u32, width: u32, height: u32, stride: u32, pixman: Pixman,
    ) -> DmabufImport {
        let stat = fstat(&memfd).unwrap();
        // 克隆 memfd 作为假的 dmabuf_fd，这样 Drop 时是安全的
        let fake_dmabuf_fd = memfd.try_clone().unwrap();

        DmabufImport {
            _guest_memfd: memfd,
            dmabuf_fd: fake_dmabuf_fd,
            dev: stat.st_dev,
            ino: stat.st_ino,
            width: NonZeroU32::new(width).unwrap(),
            height: NonZeroU32::new(height).unwrap(),
            stride: NonZeroU32::new(stride).unwrap(),
            offset,
            pixman,
        }
    }

    /// 测试 1: 验证 matches() 在完全匹配时返回 true
    #[test]
    fn test_matches_identical() {
        let memfd = memfd_create("test", MemfdFlags::CLOEXEC).unwrap();
        let stat = fstat(&memfd).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd, 0, 1920, 1080, 7680, pixman);

        assert!(mapping.matches(
            0,
            NonZeroU32::new(1920).unwrap(),
            NonZeroU32::new(1080).unwrap(),
            NonZeroU32::new(7680).unwrap(),
            pixman,
            &stat
        ));
    }

    /// 测试 2: 验证 matches() 在不同 inode 时返回 false
    #[test]
    fn test_matches_different_inode() {
        let memfd1 = memfd_create("test1", MemfdFlags::CLOEXEC).unwrap();
        let memfd2 = memfd_create("test2", MemfdFlags::CLOEXEC).unwrap();
        // 关键：两个不同的 memfd_create 调用会产生两个不同的 Inode
        let stat2 = fstat(&memfd2).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd1, 0, 1920, 1080, 7680, pixman);

        assert!(!mapping.matches(0, NonZeroU32::new(1920).unwrap(), NonZeroU32::new(1080).unwrap(), NonZeroU32::new(7680).unwrap(), pixman, &stat2));
    }

    /// 测试 3: 验证 matches() 在不同元数据时返回 false
    #[test]
    fn test_matches_different_metadata() {
        let memfd = memfd_create("test", MemfdFlags::CLOEXEC).unwrap();
        let stat = fstat(&memfd).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd, 0, 1920, 1080, 7680, pixman);

        // Inode 相同但参数不同
        let w = NonZeroU32::new(1920).unwrap();
        let h = NonZeroU32::new(1080).unwrap();
        let s = NonZeroU32::new(7680).unwrap();
        let s2 = NonZeroU32::new(10240).unwrap();
        let w2 = NonZeroU32::new(2560).unwrap();
        let h2 = NonZeroU32::new(1440).unwrap();

        assert!(mapping.matches(0, w, h, s, pixman, &stat));
        assert!(!mapping.matches(64, w, h, s, pixman, &stat));
        assert!(!mapping.matches(0, w2, h, s, pixman, &stat)); // width
        assert!(!mapping.matches(0, w, h2, s, pixman, &stat)); // height
        assert!(!mapping.matches(0, w, h, s2, pixman, &stat)); // stride
        assert!(!mapping.matches(100, w, h, s, pixman, &stat)); // offset

        let different_pixman = Pixman::from(1);
        assert!(!mapping.matches(0, w, h, s, different_pixman, &stat));
    }

    /// 测试 4: 验证缓存命中场景 (相同底层内存)
    #[test]
    fn test_cache_hit_same_memory() {
        let mut cache = ImportedTexture::new();
        let memfd1 = memfd_create("cache_test", MemfdFlags::CLOEXEC).unwrap();

        // 模拟：我们需要构造一个指向相同 Inode 的 stat。
        // 在真实 IPC 中，这是两个不同的 FD 指向同一个 Inode。
        // 在测试中，我们可以直接 dup，或者直接复用 memfd1 的 stat。
        let memfd_dup = memfd1.try_clone().unwrap();
        let stat_dup = fstat(&memfd_dup).unwrap();

        let pixman = Pixman::from(0);
        let mapping = create_mock_mapping(memfd1, 0, 1920, 1080, 7680, pixman);

        cache.buffer = Some(mapping);

        // 验证：使用 dup 出来的 FD 的 stat 应该命中缓存
        if let Some(buf) = &cache.buffer {
            let w = NonZeroU32::new(1920).unwrap();
            let h = NonZeroU32::new(1080).unwrap();
            let s = NonZeroU32::new(7680).unwrap();
            assert!(buf.matches(0, w, h, s, pixman, &stat_dup));
        }
    }

    /// 测试 5: 验证缓存未命中场景 (不同底层内存)
    #[test]
    fn test_cache_miss_different_memory() {
        let mut cache = ImportedTexture::new();

        // Create first memfd
        let memfd1 = memfd_create("miss_test1", MemfdFlags::CLOEXEC).unwrap();
        let _stat1 = fstat(&memfd1).unwrap();
        let pixman = Pixman::from(0);

        let buffer = create_mock_mapping(memfd1, 0, 1920, 1080, 7680, pixman);
        cache.buffer = Some(buffer);

        // Create a different memfd (different inode)
        let memfd2 = memfd_create("miss_test2", MemfdFlags::CLOEXEC).unwrap();
        let stat2 = fstat(&memfd2).unwrap();

        // Verify that matches returns false for different memory
        if let Some(buf) = &cache.buffer {
            let w = NonZeroU32::new(1920).unwrap();
            let h = NonZeroU32::new(1080).unwrap();
            let s = NonZeroU32::new(7680).unwrap();
            assert!(!buf.matches(0, w, h, s, pixman, &stat2));
        }
    }

    #[test]
    fn test_imported_texture_new_has_empty_state() {
        let cache = ImportedTexture::new();
        assert!(cache.texture().is_none());
        assert_eq!(cache.resolution(), (0, 0));
    }

    #[test]
    fn test_imported_texture_clear_resets_buffer_state() {
        let memfd = memfd_create("clear_test", MemfdFlags::CLOEXEC).unwrap();
        let pixman = Pixman::from(0);
        let mapping = create_mock_mapping(memfd, 0, 1024, 768, 4096, pixman);

        let mut cache = ImportedTexture::new();
        cache.buffer = Some(mapping);
        assert_eq!(cache.resolution(), (1024, 768));

        cache.clear();
        assert!(cache.texture().is_none());
        assert_eq!(cache.resolution(), (0, 0));
    }

    #[test]
    fn test_plane_offset_matches_import_offset() {
        let memfd = memfd_create("plane_offset", MemfdFlags::CLOEXEC).unwrap();
        let pixman = Pixman::from(0);
        let mapping = create_mock_mapping(memfd, 4096, 1024, 768, 4096, pixman);

        let plane = mapping.plane();

        assert_eq!(plane.offset, 4096);
        assert_eq!(plane.stride.get(), 4096);
    }
}
