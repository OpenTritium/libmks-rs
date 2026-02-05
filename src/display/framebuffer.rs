//! UDMABUF
use super::pixman_4cc::{FourCC, Pixman};
use crate::display::udma::UdmabufCreate;
use relm4::gtk::gdk::{DmabufTextureBuilder, Texture};
use rustix::{
    fs::{MemfdFlags, Mode, OFlags, SealFlags, fcntl_add_seals, ftruncate, memfd_create, open},
    ioctl::ioctl,
    mm::{MapFlags, ProtFlags, mmap, munmap},
    param::page_size,
};
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    ptr::{self, NonNull},
};

const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// 封装一个 Memfd 共享内存块
#[derive(Debug)]
pub struct ShmBuffer {
    _mem_fd: OwnedFd,
    dmabuf_fd: OwnedFd,
    pub ptr: NonNull<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixman: Pixman,
}

impl ShmBuffer {
    #[inline]
    pub fn with_buf(width: u32, height: u32, stride: usize, pixman: Pixman, buf: &[u8]) -> io::Result<Self> {
        let this = Self::new(width, height, stride, pixman)?;
        debug_assert_eq!(this.len(), buf.len());
        unsafe {
            buf.as_ptr().copy_to_nonoverlapping(this.ptr.as_ptr(), buf.len());
        }
        Ok(this)
    }

    #[inline]
    pub fn new(width: u32, height: u32, stride: usize, pixman: Pixman) -> io::Result<Self> {
        let size = stride as u64 * height as u64;
        let mem_fd = memfd_create("qemu_surface", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?; // 允许添加封印
        let page_size = page_size() as u64;
        let aligned_size = (size + page_size - 1) & !(page_size - 1);
        ftruncate(&mem_fd, aligned_size)?;
        let seals = SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL; //  禁止变大、变小、重新封印
        fcntl_add_seals(&mem_fd, seals)?;
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                aligned_size as usize,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &mem_fd,
                0,
            )?
        }; // 我们要让显卡拿到fd，所以不用匿名映射
        let udmabuf_dev = open("/dev/udmabuf", OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())?;
        let create_udmabuf = UdmabufCreate::new(mem_fd.as_raw_fd(), aligned_size);
        let dmabuf_fd = unsafe { ioctl(&udmabuf_dev, create_udmabuf)? };
        Ok(Self {
            _mem_fd: mem_fd,
            dmabuf_fd,
            ptr: unsafe { NonNull::new_unchecked(ptr as *mut u8) },
            width,
            height,
            stride,
            pixman,
        })
    }

    #[inline]
    pub fn try_clone(&self) -> io::Result<Self> {
        let this = Self::new(self.width, self.height, self.stride, self.pixman)?;
        unsafe {
            this.ptr.copy_from_nonoverlapping(self.ptr, self.len());
        }
        Ok(this)
    }

    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub const fn len(&self) -> usize { self.height as usize * self.stride }

    #[inline]
    pub fn dmabuf_fd(&self) -> RawFd { self.dmabuf_fd.as_raw_fd() }

    /// 此函数只关心缓冲区（大小）是否可以被重用，所以你最好调用此函数之后更新一下宽高和步幅
    #[inline]
    pub const fn is_reuseable(&self, height: u32, stride: usize) -> bool { self.len() == height as usize * stride }

    /// 警告：你必须先检查 is_reuseable，再调用此函数
    /// 同时更新 pixman 和 buf
    #[inline]
    pub fn update_all(&mut self, width: u32, height: u32, stride: usize, pixman: Pixman, buf: &[u8]) {
        debug_assert_eq!(self.len(), buf.len());
        self.pixman = pixman;
        self.width = width;
        self.height = height;
        self.stride = stride;
        unsafe {
            buf.as_ptr().copy_to_nonoverlapping(self.ptr.as_ptr(), buf.len());
        }
    }

    /// 警告：正常情况下 scanout 事件后一定跟着 update，但是你最好在调用此函数之前断言一下前后的 pixman 是否一致
    /// 给定一个起始点，矩形宽高，我会更新这块区域
    #[inline]
    pub fn update_rect(&mut self, x: u32, y: u32, width: u32, height: u32, stride: usize, buf: *const u8) {
        debug_assert!((x + width) <= self.width, "宽度不能越界");
        debug_assert!((y + height) <= self.height, "高度不能越界");
        let bpp = self.pixman.bytes_per_pixel(); // 我们假设更新部分区域的时候 pixman 一定是被初始化的
        let row_bytes = width as usize * bpp; // 我们要覆盖的脏矩形每行多少有效字节
        let dest = self.ptr.as_ptr();
        let src = buf;
        let h = height as usize;
        let x = x as usize;
        let y = y as usize;
        for i in 0..h {
            // 我们先计算行x步幅跳转到我们要处理的行，然后再跳转到我们要处理的列
            // dst = base + (y+i)*stride + x*bpp
            let dst_offset = ((y + i) * self.stride) + (x * bpp);
            // 这个就很简单，我们不用管坐标和宽
            // src = i * src_stride
            let src_offset = i * stride;
            unsafe {
                ptr::copy_nonoverlapping(src.add(src_offset), dest.add(dst_offset), row_bytes);
            }
        }
    }

    /// 此函数于获取上个画面（active）的脏块并执行区域更新
    /// 我们约定全屏更新设置全屏 damage, 这是因为当 active 收到 scanout 事件的时候不一定会重新创建缓冲区，
    /// 我们 shadow 的大小几乎与 active 的大小一致，所以 shadow 也可以不用重新创建缓冲区，
    /// 此时就应该让 active 指定 damage 为全画幅
    #[inline]
    pub fn update_damage(&mut self, d: Damage, active: &Self) {
        // 首先我们先断言下画幅是否一致
        debug_assert!(self.is_reuseable(active.height, active.stride));
        // 然后我们断言下 pixman 因为它影响 bpp
        debug_assert!(self.pixman == active.pixman);
        let Damage { x, y, w, h } = d;
        let bpp = self.pixman.bytes_per_pixel();
        // 计算源数据(active)中脏矩形左上角的偏移量
        // Offset = (y * stride) + (x * bpp)
        let offset = (y as usize * active.stride) + (x as usize * bpp);
        let src_ptr = unsafe { active.ptr.add(offset) };
        self.update_rect(x, y, w, h, active.stride, src_ptr.as_ptr());
    }
}

impl Drop for ShmBuffer {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.ptr.as_ptr() as *mut _, self.len());
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Damage {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Debug)]
pub struct Swapchain {
    buffers: [Option<ShmBuffer>; 2],
    texture_cache: [Option<Texture>; 2],
    active_frame: usize,
    last_damage: Option<Damage>, // 记录上一帧的脏矩形
}

impl Default for Swapchain {
    fn default() -> Self { Self::new() }
}

impl Swapchain {
    #[inline]
    pub const fn new() -> Self {
        Self { buffers: [None, None], active_frame: 0, texture_cache: [None, None], last_damage: None }
    }

    #[inline]
    const fn active_idx(&self) -> usize { self.active_frame % 2 }

    #[inline]
    const fn shadow_idx(&self) -> usize { self.active_idx() ^ 1 }

    #[inline]
    pub fn active_texture(&self) -> &Option<Texture> { unsafe { self.texture_cache.get_unchecked(self.active_idx()) } }

    #[inline]
    pub fn active_texture_mut(&mut self) -> &mut Option<Texture> {
        let active_idx = self.active_idx();
        unsafe { self.texture_cache.get_unchecked_mut(active_idx) }
    }

    #[inline]
    pub fn active_buf(&self) -> &Option<ShmBuffer> { unsafe { self.buffers.get_unchecked(self.active_idx()) } }

    #[inline]
    pub fn shadow_buf(&self) -> &Option<ShmBuffer> { unsafe { self.buffers.get_unchecked(self.shadow_idx()) } }

    #[inline]
    pub fn shadow_buf_mut(&mut self) -> &mut Option<ShmBuffer> {
        let shadow_idx = self.shadow_idx();
        unsafe { self.buffers.get_unchecked_mut(shadow_idx) }
    }

    /// 返回宽度 x 高度
    #[inline]
    pub fn resolution(&self) -> Option<(u32, u32)> { self.active_buf().as_ref().map(|b| (b.width, b.height)) }

    /// 将活跃的帧同步到影子缓冲区
    /// 此函数的副作用是，如果影子缓冲区没有创建（或不可重用），会自动帮你创建
    /// 返回值表示是否创建了新的缓冲区，你可以用它判断是否该刷新纹理
    #[inline]
    pub fn sync_active_to_shadow(&mut self) -> io::Result<bool> {
        // 如果有脏块就只同步脏区域
        let active_idx = self.active_idx();
        let shadow_idx = self.shadow_idx();
        let [dst, src] = unsafe { self.buffers.get_disjoint_unchecked_mut([shadow_idx, active_idx]) };
        let src = match src {
            Some(src) => src,
            None => {
                return Err(io::Error::other("active buffer not initialized"));
            }
        };
        if let Some(d) = self.last_damage {
            match dst {
                Some(dst) if dst.is_reuseable(src.height, src.stride) => {
                    // 如果两方都有缓存并且可以重用就不用重新创建缓冲区，直接执行更新
                    dst.width = src.width;
                    dst.height = src.height;
                    dst.stride = src.stride;
                    dst.pixman = src.pixman;
                    dst.update_damage(d, src);
                    return Ok(false);
                }
                _ => {
                    // 我们就别管脏块了，直接全部克隆
                    let src_clone = src.try_clone()?;
                    *dst = Some(src_clone);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// 切换影子缓冲区为活跃缓冲区
    #[inline]
    pub const fn swap_frame(&mut self) { self.active_frame = self.active_frame.wrapping_add(1) }

    /// 在这里我们需要将整个画面标记为脏块
    #[inline]
    pub fn full_update_texture(
        &mut self, width: u32, height: u32, stride: u32, pixman: Pixman, buf: &[u8],
    ) -> anyhow::Result<Texture> {
        let mk_texture = |fd: RawFd, fourcc: FourCC| unsafe {
            DmabufTextureBuilder::new()
                .set_width(width)
                .set_height(height)
                .set_n_planes(1)
                .set_stride(0, stride)
                .set_offset(0, 0)
                .set_fourcc(fourcc.into())
                .set_modifier(DRM_FORMAT_MOD_LINEAR)
                .set_fd(0, fd)
                .build()
        };
        // 我们总是无条件通知后面的人说我执行了全量刷新
        self.last_damage = Some(Damage { x: 0, y: 0, w: width, h: height });
        let stride = stride as usize;
        // 我们先检查缓冲区是否被初始化
        let Some(shadow_buf) = self.shadow_buf_mut() else {
            // 缓冲没被初始化说明纹理也没有
            let buf = ShmBuffer::with_buf(width, height, stride, pixman, buf)?;
            let active_buf = self.shadow_buf_mut().insert(buf);
            let fourcc: FourCC = pixman.try_into()?;
            let texture = mk_texture(active_buf.dmabuf_fd(), fourcc)?;
            self.swap_frame();
            *self.active_texture_mut() = Some(texture.clone());
            return Ok(texture);
        };
        // 缓冲区已经被初始化，检查是否可以被重用
        let is_reuseable = shadow_buf.is_reuseable(height, stride);
        if !is_reuseable {
            // 重新初始化缓冲
            let buf = self.shadow_buf_mut().insert(ShmBuffer::with_buf(width, height, stride, pixman, buf)?);
            let fourcc: FourCC = pixman.try_into()?;
            let texture = mk_texture(buf.dmabuf_fd(), fourcc)?;
            self.swap_frame();
            // 重新缓存纹理
            *self.active_texture_mut() = Some(texture.clone());
            return Ok(texture);
        }
        // 缓冲可重用
        shadow_buf.height = height;
        shadow_buf.stride = stride;
        shadow_buf.width = width;
        shadow_buf.pixman = pixman;
        unsafe {
            buf.as_ptr().copy_to_nonoverlapping(shadow_buf.ptr.as_ptr(), buf.len());
        }
        // 我们假定 buf 初始化后 纹理一定被初始化
        self.swap_frame();
        let texture = self.active_texture().as_ref().cloned().unwrap();
        Ok(texture)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn partial_update_texture(
        &mut self, x: u32, y: u32, width: u32, height: u32, stride: u32, pixman: Pixman, buf: &[u8],
    ) -> anyhow::Result<Texture> {
        debug_assert!(height > 0);
        debug_assert!(stride > 0);
        debug_assert!(width > 0);
        // 基于上一帧断言这次更新
        if self.active_buf().is_none() {
            return Err(io::Error::other("active buf shoudle be initialized at first").into());
        }
        debug_assert_eq!(pixman, self.active_buf().as_ref().unwrap().pixman);
        // 这里可能会创建新的影子缓冲区
        let need_refresh_texture = self.sync_active_to_shadow()?; // 先同步旧的脏块
        self.last_damage = Some(Damage { x, y, w: width, h: height }); // 再宣告新的脏块
        let shadow_buf = unsafe { self.shadow_buf_mut().as_mut().unwrap_unchecked() }; // 此时影子缓冲区一定存在
        shadow_buf.update_rect(x, y, width, height, stride as usize, buf.as_ptr());
        let fourcc: FourCC = pixman.try_into()?;
        if need_refresh_texture {
            let texture = unsafe {
                DmabufTextureBuilder::new()
                    .set_width(shadow_buf.width)
                    .set_height(shadow_buf.height)
                    .set_n_planes(1)
                    .set_stride(0, shadow_buf.stride as u32)
                    .set_offset(0, 0)
                    .set_fourcc(fourcc.into())
                    .set_modifier(DRM_FORMAT_MOD_LINEAR)
                    .set_fd(0, shadow_buf.dmabuf_fd())
                    .build()?
            };
            self.swap_frame();
            *self.active_texture_mut() = Some(texture.clone());
            return Ok(texture);
        }
        self.swap_frame();
        let texture = self.active_texture().as_ref().cloned().unwrap();
        Ok(texture)
    }
}
