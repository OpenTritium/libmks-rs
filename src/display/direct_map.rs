use super::{
    Error,
    pixman_4cc::{FourCC, Pixman},
    udma::{DRM_FORMAT_MOD_LINEAR, DmabufPlane, build_dmabuf_texture_planar, create_udmabuf_fd},
};
use relm4::gtk::gdk::Texture;
use rustix::fs::{Stat, fstat};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

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
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub offset: u32,
    pub pixman: Pixman,
}

impl DmabufImport {
    /// Creates a new mapping, capturing file identity for caching.
    #[inline]
    pub fn new(
        memfd: OwnedFd, offset: u32, width: u32, height: u32, stride: u32, pixman: Pixman,
    ) -> Result<Self, Error> {
        // Capture file identity (inode) to detect reuse of the same physical memory.
        let stat = fstat(&memfd)?;
        let size = (offset as u64) + (height as u64 * stride as u64);
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
    pub fn matches(&self, stat: &Stat, offset: u32, width: u32, height: u32, stride: u32, pixman: Pixman) -> bool {
        self.dev == stat.st_dev
            && self.ino == stat.st_ino
            && self.offset == offset
            && self.width == width
            && self.height == height
            && self.stride == stride
            && self.pixman == pixman
    }

    #[inline]
    pub fn as_raw_dmabuf_fd(&self) -> RawFd { self.dmabuf_fd.as_raw_fd() }
}

#[derive(Debug)]
pub struct ImportedTexture {
    buffer: Option<DmabufImport>,
    texture: Option<Texture>,
}

impl Default for ImportedTexture {
    fn default() -> Self { Self::new() }
}

impl ImportedTexture {
    #[inline]
    pub const fn new() -> Self { Self { buffer: None, texture: None } }

    /// Updates the texture, reusing the existing mapping if the underlying memory object (inode) matches.
    #[inline]
    pub fn update_texture(
        &mut self, memfd: OwnedFd, offset: u32, width: u32, height: u32, stride: u32, pixman_format: u32,
    ) -> Result<Texture, Error> {
        let pixman = Pixman::from(pixman_format);
        // Check file identity. QEMU may send different FDs for the same underlying memory.
        let stat = fstat(&memfd)?;
        // Attempt to reuse existing mapping
        if let Some(buf) = &self.buffer
            && buf.matches(&stat, offset, width, height, stride, pixman)
        {
            // Cache hit: Reuse the existing texture.
            // `memfd` is dropped here, closing the duplicate FD, which is correct.
            return Ok(self.texture.as_ref().expect("Texture missing in cache").clone());
        }
        // Cache miss: Create new mapping and texture
        let fourcc: FourCC = pixman.try_into()?;
        let buffer = DmabufImport::new(memfd, offset, width, height, stride, pixman)?;
        let plane = DmabufPlane { fd: buffer.as_raw_dmabuf_fd(), stride, offset: 0 };
        let texture = build_dmabuf_texture_planar(width, height, fourcc, DRM_FORMAT_MOD_LINEAR, &[plane], None, None)?;
        self.buffer = Some(buffer);
        self.texture = Some(texture.clone());
        Ok(texture)
    }

    #[inline]
    pub const fn texture(&self) -> Option<&Texture> { self.texture.as_ref() }

    /// Returns the resolution (width, height) of the current buffer.
    #[inline]
    pub fn resolution(&self) -> Option<(u32, u32)> { self.buffer.as_ref().map(|b| (b.width, b.height)) }

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
    fn create_mock_mapping(memfd: OwnedFd, width: u32, height: u32, stride: u32, pixman: Pixman) -> DmabufImport {
        let stat = fstat(&memfd).unwrap();
        // 克隆 memfd 作为假的 dmabuf_fd，这样 Drop 时是安全的
        let fake_dmabuf_fd = memfd.try_clone().unwrap();

        DmabufImport {
            _guest_memfd: memfd,
            dmabuf_fd: fake_dmabuf_fd,
            dev: stat.st_dev,
            ino: stat.st_ino,
            width,
            height,
            stride,
            offset: 0,
            pixman,
        }
    }

    /// 测试 1: 验证 matches() 在完全匹配时返回 true
    #[test]
    fn test_matches_identical() {
        let memfd = memfd_create("test", MemfdFlags::CLOEXEC).unwrap();
        let stat = fstat(&memfd).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd, 1920, 1080, 7680, pixman);

        assert!(mapping.matches(&stat, 0, 1920, 1080, 7680, pixman));
    }

    /// 测试 2: 验证 matches() 在不同 inode 时返回 false
    #[test]
    fn test_matches_different_inode() {
        let memfd1 = memfd_create("test1", MemfdFlags::CLOEXEC).unwrap();
        let memfd2 = memfd_create("test2", MemfdFlags::CLOEXEC).unwrap();
        // 关键：两个不同的 memfd_create 调用会产生两个不同的 Inode
        let stat2 = fstat(&memfd2).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd1, 1920, 1080, 7680, pixman);

        assert!(!mapping.matches(&stat2, 0, 1920, 1080, 7680, pixman));
    }

    /// 测试 3: 验证 matches() 在不同元数据时返回 false
    #[test]
    fn test_matches_different_metadata() {
        let memfd = memfd_create("test", MemfdFlags::CLOEXEC).unwrap();
        let stat = fstat(&memfd).unwrap();
        let pixman = Pixman::from(0);

        let mapping = create_mock_mapping(memfd, 1920, 1080, 7680, pixman);

        // Inode 相同但参数不同
        assert!(!mapping.matches(&stat, 0, 2560, 1080, 7680, pixman)); // width
        assert!(!mapping.matches(&stat, 0, 1920, 1440, 7680, pixman)); // height
        assert!(!mapping.matches(&stat, 0, 1920, 1080, 10240, pixman)); // stride
        assert!(!mapping.matches(&stat, 100, 1920, 1080, 7680, pixman)); // offset

        let different_pixman = Pixman::from(1);
        assert!(!mapping.matches(&stat, 0, 1920, 1080, 7680, different_pixman));
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
        let mapping = create_mock_mapping(memfd1, 1920, 1080, 7680, pixman);

        cache.buffer = Some(mapping);

        // 验证：使用 dup 出来的 FD 的 stat 应该命中缓存
        if let Some(buf) = &cache.buffer {
            assert!(buf.matches(&stat_dup, 0, 1920, 1080, 7680, pixman));
        }
    }

    /// 测试 5: 验证缓存未命中场景 (不同底层内存)
    #[test]
    fn test_cache_miss_different_memory() {
        let mut cache = ImportedTexture::new();

        // Create first memfd
        let memfd1 = memfd_create("miss_test1", MemfdFlags::CLOEXEC).unwrap();
        let pixman = Pixman::from(0);

        let buffer = create_mock_mapping(memfd1, 1920, 1080, 7680, pixman);
        cache.buffer = Some(buffer);

        // Create a different memfd (different inode)
        let memfd2 = memfd_create("miss_test2", MemfdFlags::CLOEXEC).unwrap();
        let stat2 = fstat(&memfd2).unwrap();

        // Verify that matches returns false for different memory
        if let Some(buf) = &cache.buffer {
            assert!(!buf.matches(&stat2, 0, 1920, 1080, 7680, pixman));
        }
    }

    #[test]
    fn test_imported_texture_new_has_empty_state() {
        let cache = ImportedTexture::new();
        assert!(cache.texture().is_none());
        assert!(cache.resolution().is_none());
    }

    #[test]
    fn test_imported_texture_clear_resets_buffer_state() {
        let memfd = memfd_create("clear_test", MemfdFlags::CLOEXEC).unwrap();
        let pixman = Pixman::from(0);
        let mapping = create_mock_mapping(memfd, 1024, 768, 4096, pixman);

        let mut cache = ImportedTexture::new();
        cache.buffer = Some(mapping);
        assert_eq!(cache.resolution(), Some((1024, 768)));

        cache.clear();
        assert!(cache.texture().is_none());
        assert!(cache.resolution().is_none());
    }
}
