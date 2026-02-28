/// include/uapi/drm/drm_fourcc
pub mod drm_4cc {
    use derive_more::{Deref, From, Into};

    #[derive(Copy, Clone, PartialEq, Eq, Deref, From, Debug, Into)]
    pub struct FourCC(u32);

    #[inline]
    pub(crate) const fn fourcc_code(a: u8, b: u8, c: u8, d: u8) -> FourCC {
        FourCC((a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24))
    }

    pub const INVALID: FourCC = FourCC(0);
    pub const C1: FourCC = fourcc_code(b'C', b'1', b' ', b' ');
    pub const C2: FourCC = fourcc_code(b'C', b'2', b' ', b' ');
    pub const C4: FourCC = fourcc_code(b'C', b'4', b' ', b' ');
    pub const C8: FourCC = fourcc_code(b'C', b'8', b' ', b' ');
    pub const D1: FourCC = fourcc_code(b'D', b'1', b' ', b' ');
    pub const D2: FourCC = fourcc_code(b'D', b'2', b' ', b' ');
    pub const D4: FourCC = fourcc_code(b'D', b'4', b' ', b' ');
    pub const D8: FourCC = fourcc_code(b'D', b'8', b' ', b' ');
    pub const R1: FourCC = fourcc_code(b'R', b'1', b' ', b' ');
    pub const R2: FourCC = fourcc_code(b'R', b'2', b' ', b' ');
    pub const R4: FourCC = fourcc_code(b'R', b'4', b' ', b' ');
    pub const R8: FourCC = fourcc_code(b'R', b'8', b' ', b' ');
    pub const R10: FourCC = fourcc_code(b'R', b'1', b'0', b' ');
    pub const R12: FourCC = fourcc_code(b'R', b'1', b'2', b' ');
    pub const R16: FourCC = fourcc_code(b'R', b'1', b'6', b' ');
    pub const RG88: FourCC = fourcc_code(b'R', b'G', b'8', b'8');
    pub const GR88: FourCC = fourcc_code(b'G', b'R', b'8', b'8');
    pub const RG1616: FourCC = fourcc_code(b'R', b'G', b'3', b'2');
    pub const GR1616: FourCC = fourcc_code(b'G', b'R', b'3', b'2');
    pub const RGB332: FourCC = fourcc_code(b'R', b'G', b'B', b'8');
    pub const BGR233: FourCC = fourcc_code(b'B', b'G', b'R', b'8');
    pub const XRGB4444: FourCC = fourcc_code(b'X', b'R', b'1', b'2');
    pub const XBGR4444: FourCC = fourcc_code(b'X', b'B', b'1', b'2');
    pub const RGBX4444: FourCC = fourcc_code(b'R', b'X', b'1', b'2');
    pub const BGRX4444: FourCC = fourcc_code(b'B', b'X', b'1', b'2');
    pub const ARGB4444: FourCC = fourcc_code(b'A', b'R', b'1', b'2');
    pub const ABGR4444: FourCC = fourcc_code(b'A', b'B', b'1', b'2');
    pub const RGBA4444: FourCC = fourcc_code(b'R', b'A', b'1', b'2');
    pub const BGRA4444: FourCC = fourcc_code(b'B', b'A', b'1', b'2');
    pub const XRGB1555: FourCC = fourcc_code(b'X', b'R', b'1', b'5');
    pub const XBGR1555: FourCC = fourcc_code(b'X', b'B', b'1', b'5');
    pub const RGBX5551: FourCC = fourcc_code(b'R', b'X', b'1', b'5');
    pub const BGRX5551: FourCC = fourcc_code(b'B', b'X', b'1', b'5');
    pub const ARGB1555: FourCC = fourcc_code(b'A', b'R', b'1', b'5');
    pub const ABGR1555: FourCC = fourcc_code(b'A', b'B', b'1', b'5');
    pub const RGBA5551: FourCC = fourcc_code(b'R', b'A', b'1', b'5');
    pub const BGRA5551: FourCC = fourcc_code(b'B', b'A', b'1', b'5');
    pub const RGB565: FourCC = fourcc_code(b'R', b'G', b'1', b'6');
    pub const BGR565: FourCC = fourcc_code(b'B', b'G', b'1', b'6');
    pub const RGB888: FourCC = fourcc_code(b'R', b'G', b'2', b'4');
    pub const BGR888: FourCC = fourcc_code(b'B', b'G', b'2', b'4');
    pub const XRGB8888: FourCC = fourcc_code(b'X', b'R', b'2', b'4');
    pub const XBGR8888: FourCC = fourcc_code(b'X', b'B', b'2', b'4');
    pub const RGBX8888: FourCC = fourcc_code(b'R', b'X', b'2', b'4');
    pub const BGRX8888: FourCC = fourcc_code(b'B', b'X', b'2', b'4');
    pub const ARGB8888: FourCC = fourcc_code(b'A', b'R', b'2', b'4');
    pub const ABGR8888: FourCC = fourcc_code(b'A', b'B', b'2', b'4');
    pub const RGBA8888: FourCC = fourcc_code(b'R', b'A', b'2', b'4');
    pub const BGRA8888: FourCC = fourcc_code(b'B', b'A', b'2', b'4');
    pub const XRGB2101010: FourCC = fourcc_code(b'X', b'R', b'3', b'0');
    pub const XBGR2101010: FourCC = fourcc_code(b'X', b'B', b'3', b'0');
    pub const RGBX1010102: FourCC = fourcc_code(b'R', b'X', b'3', b'0');
    pub const BGRX1010102: FourCC = fourcc_code(b'B', b'X', b'3', b'0');
    pub const ARGB2101010: FourCC = fourcc_code(b'A', b'R', b'3', b'0');
    pub const ABGR2101010: FourCC = fourcc_code(b'A', b'B', b'3', b'0');
    pub const RGBA1010102: FourCC = fourcc_code(b'R', b'A', b'3', b'0');
    pub const BGRA1010102: FourCC = fourcc_code(b'B', b'A', b'3', b'0');
    pub const RGB161616: FourCC = fourcc_code(b'R', b'G', b'4', b'8');
    pub const BGR161616: FourCC = fourcc_code(b'B', b'G', b'4', b'8');
    pub const XRGB16161616: FourCC = fourcc_code(b'X', b'R', b'4', b'8');
    pub const XBGR16161616: FourCC = fourcc_code(b'X', b'B', b'4', b'8');
    pub const ARGB16161616: FourCC = fourcc_code(b'A', b'R', b'4', b'8');
    pub const ABGR16161616: FourCC = fourcc_code(b'A', b'B', b'4', b'8');
    pub const XRGB16161616F: FourCC = fourcc_code(b'X', b'R', b'4', b'H');
    pub const XBGR16161616F: FourCC = fourcc_code(b'X', b'B', b'4', b'H');
    pub const ARGB16161616F: FourCC = fourcc_code(b'A', b'R', b'4', b'H');
    pub const ABGR16161616F: FourCC = fourcc_code(b'A', b'B', b'4', b'H');
    pub const R16F: FourCC = fourcc_code(b'R', b' ', b' ', b'H');
    pub const GR1616F: FourCC = fourcc_code(b'G', b'R', b' ', b'H');
    pub const BGR161616F: FourCC = fourcc_code(b'B', b'G', b'R', b'H');
    pub const R32F: FourCC = fourcc_code(b'R', b' ', b' ', b'F');
    pub const GR3232F: FourCC = fourcc_code(b'G', b'R', b' ', b'F');
    pub const BGR323232F: FourCC = fourcc_code(b'B', b'G', b'R', b'F');
    pub const ABGR32323232F: FourCC = fourcc_code(b'A', b'B', b'8', b'F');
    pub const AXBXGXRX106106106106: FourCC = fourcc_code(b'A', b'B', b'1', b'0');
    pub const YUYV: FourCC = fourcc_code(b'Y', b'U', b'Y', b'V');
    pub const YVYU: FourCC = fourcc_code(b'Y', b'V', b'Y', b'U');
    pub const UYVY: FourCC = fourcc_code(b'U', b'Y', b'V', b'Y');
    pub const VYUY: FourCC = fourcc_code(b'V', b'Y', b'U', b'Y');
    pub const AYUV: FourCC = fourcc_code(b'A', b'Y', b'U', b'V');
    pub const AVUY8888: FourCC = fourcc_code(b'A', b'V', b'U', b'Y');
    pub const XYUV8888: FourCC = fourcc_code(b'X', b'Y', b'U', b'V');
    pub const XVUY8888: FourCC = fourcc_code(b'X', b'V', b'U', b'Y');
    pub const VUY888: FourCC = fourcc_code(b'V', b'U', b'2', b'4');
    pub const VUY101010: FourCC = fourcc_code(b'V', b'U', b'3', b'0');
    pub const Y210: FourCC = fourcc_code(b'Y', b'2', b'1', b'0');
    pub const Y212: FourCC = fourcc_code(b'Y', b'2', b'1', b'2');
    pub const Y216: FourCC = fourcc_code(b'Y', b'2', b'1', b'6');
    pub const Y410: FourCC = fourcc_code(b'Y', b'4', b'1', b'0');
    pub const Y412: FourCC = fourcc_code(b'Y', b'4', b'1', b'2');
    pub const Y416: FourCC = fourcc_code(b'Y', b'4', b'1', b'6');
    pub const XVYU2101010: FourCC = fourcc_code(b'X', b'V', b'3', b'0');
    pub const XVYU12_16161616: FourCC = fourcc_code(b'X', b'V', b'3', b'6');
    pub const XVYU16161616: FourCC = fourcc_code(b'X', b'V', b'4', b'8');
    pub const Y0L0: FourCC = fourcc_code(b'Y', b'0', b'L', b'0');
    pub const X0L0: FourCC = fourcc_code(b'X', b'0', b'L', b'0');
    pub const Y0L2: FourCC = fourcc_code(b'Y', b'0', b'L', b'2');
    pub const X0L2: FourCC = fourcc_code(b'X', b'0', b'L', b'2');
    pub const YUV420_8BIT: FourCC = fourcc_code(b'Y', b'U', b'0', b'8');
    pub const YUV420_10BIT: FourCC = fourcc_code(b'Y', b'U', b'1', b'0');
    pub const XRGB8888_A8: FourCC = fourcc_code(b'X', b'R', b'A', b'8');
    pub const XBGR8888_A8: FourCC = fourcc_code(b'X', b'B', b'A', b'8');
    pub const RGBX8888_A8: FourCC = fourcc_code(b'R', b'X', b'A', b'8');
    pub const BGRX8888_A8: FourCC = fourcc_code(b'B', b'X', b'A', b'8');
    pub const RGB888_A8: FourCC = fourcc_code(b'R', b'8', b'A', b'8');
    pub const BGR888_A8: FourCC = fourcc_code(b'B', b'8', b'A', b'8');
    pub const RGB565_A8: FourCC = fourcc_code(b'R', b'5', b'A', b'8');
    pub const BGR565_A8: FourCC = fourcc_code(b'B', b'5', b'A', b'8');
    pub const NV12: FourCC = fourcc_code(b'N', b'V', b'1', b'2');
    pub const NV21: FourCC = fourcc_code(b'N', b'V', b'2', b'1');
    pub const NV16: FourCC = fourcc_code(b'N', b'V', b'1', b'6');
    pub const NV61: FourCC = fourcc_code(b'N', b'V', b'6', b'1');
    pub const NV24: FourCC = fourcc_code(b'N', b'V', b'2', b'4');
    pub const NV42: FourCC = fourcc_code(b'N', b'V', b'4', b'2');
    pub const NV15: FourCC = fourcc_code(b'N', b'V', b'1', b'5');
    pub const NV20: FourCC = fourcc_code(b'N', b'V', b'2', b'0');
    pub const NV30: FourCC = fourcc_code(b'N', b'V', b'3', b'0');
    pub const P210: FourCC = fourcc_code(b'P', b'2', b'1', b'0');
    pub const P010: FourCC = fourcc_code(b'P', b'0', b'1', b'0');
    pub const P012: FourCC = fourcc_code(b'P', b'0', b'1', b'2');
    pub const P016: FourCC = fourcc_code(b'P', b'0', b'1', b'6');
    pub const P030: FourCC = fourcc_code(b'P', b'0', b'3', b'0');
    pub const Q410: FourCC = fourcc_code(b'Q', b'4', b'1', b'0');
    pub const Q401: FourCC = fourcc_code(b'Q', b'4', b'0', b'1');
    pub const S010: FourCC = fourcc_code(b'S', b'0', b'1', b'0');
    pub const S210: FourCC = fourcc_code(b'S', b'2', b'1', b'0');
    pub const S410: FourCC = fourcc_code(b'S', b'4', b'1', b'0');
    pub const S012: FourCC = fourcc_code(b'S', b'0', b'1', b'2');
    pub const S212: FourCC = fourcc_code(b'S', b'2', b'1', b'2');
    pub const S412: FourCC = fourcc_code(b'S', b'4', b'1', b'2');
    pub const S016: FourCC = fourcc_code(b'S', b'0', b'1', b'6');
    pub const S216: FourCC = fourcc_code(b'S', b'2', b'1', b'6');
    pub const S416: FourCC = fourcc_code(b'S', b'4', b'1', b'6');
    pub const YUV410: FourCC = fourcc_code(b'Y', b'U', b'V', b'9');
    pub const YVU410: FourCC = fourcc_code(b'Y', b'V', b'U', b'9');
    pub const YUV411: FourCC = fourcc_code(b'Y', b'U', b'1', b'1');
    pub const YVU411: FourCC = fourcc_code(b'Y', b'V', b'1', b'1');
    pub const YUV420: FourCC = fourcc_code(b'Y', b'U', b'1', b'2');
    pub const YVU420: FourCC = fourcc_code(b'Y', b'V', b'1', b'2');
    pub const YUV422: FourCC = fourcc_code(b'Y', b'U', b'1', b'6');
    pub const YVU422: FourCC = fourcc_code(b'Y', b'V', b'1', b'6');
    pub const YUV444: FourCC = fourcc_code(b'Y', b'U', b'2', b'4');
    pub const YVU444: FourCC = fourcc_code(b'Y', b'V', b'2', b'4');
}

/// from libpixman
pub mod pixman {
    use derive_more::{From, Into};

    #[derive(Debug, From, Clone, Copy, Into, PartialEq, Eq)]
    pub struct Pixman(u32);

    impl Pixman {
        #[inline]
        pub const fn is_premultiplied(&self) -> bool {
            matches!(
                *self,
                A8R8G8B8
                    | A8B8G8R8
                    | B8G8R8A8
                    | R8G8B8A8
                    | A2R2G2B2
                    | A2B2G2R2
                    | A1R1G1B1
                    | A1B1G1R1
                    | A2R10G10B10
                    | A2B10G10R10
                    | RGBA_FLOAT
                    | A1R5G5B5
                    | A1B5G5R5
                    | A4R4G4B4
                    | A4B4G4R4
                    | A8
                    | A4
                    | A1
            )
        }

        #[inline]
        pub const fn bytes_per_pixel(&self) -> usize {
            let Pixman(raw) = *self;
            let bpp = ((raw >> 24) & 0xFF) as usize;
            bpp.div_ceil(8)
        }
    }

    pub const RGBA_FLOAT: Pixman = Pixman(281756740);
    pub const RGB_FLOAT: Pixman = Pixman(214631492);
    pub const A8R8G8B8: Pixman = Pixman(537036936);
    pub const X8R8G8B8: Pixman = Pixman(537004168);
    pub const A8B8G8R8: Pixman = Pixman(537102472);
    pub const X8B8G8R8: Pixman = Pixman(537069704);
    pub const B8G8R8A8: Pixman = Pixman(537430152);
    pub const B8G8R8X8: Pixman = Pixman(537397384);
    pub const R8G8B8A8: Pixman = Pixman(537495688);
    pub const R8G8B8X8: Pixman = Pixman(537462920);
    pub const X14R6G6B6: Pixman = Pixman(537003622);
    pub const X2R10G10B10: Pixman = Pixman(537004714);
    pub const A2R10G10B10: Pixman = Pixman(537012906);
    pub const X2B10G10R10: Pixman = Pixman(537070250);
    pub const A2B10G10R10: Pixman = Pixman(537078442);
    pub const A8R8G8B8_S_RGB: Pixman = Pixman(537561224);
    pub const R8G8B8: Pixman = Pixman(402786440);
    pub const B8G8R8: Pixman = Pixman(402851976);
    pub const R5G6B5: Pixman = Pixman(268567909);
    pub const B5G6R5: Pixman = Pixman(268633445);
    pub const A1R5G5B5: Pixman = Pixman(268571989);
    pub const X1R5G5B5: Pixman = Pixman(268567893);
    pub const A1B5G5R5: Pixman = Pixman(268637525);
    pub const X1B5G5R5: Pixman = Pixman(268633429);
    pub const A4R4G4B4: Pixman = Pixman(268584004);
    pub const X4R4G4B4: Pixman = Pixman(268567620);
    pub const A4B4G4R4: Pixman = Pixman(268649540);
    pub const X4B4G4R4: Pixman = Pixman(268633156);
    pub const A8: Pixman = Pixman(134316032);
    pub const R3G3B2: Pixman = Pixman(134349618);
    pub const B2G3R3: Pixman = Pixman(134415154);
    pub const A2R2G2B2: Pixman = Pixman(134357538);
    pub const A2B2G2R2: Pixman = Pixman(134423074);
    pub const C8: Pixman = Pixman(134479872);
    pub const G8: Pixman = Pixman(134545408);
    pub const X4A4: Pixman = Pixman(134299648);
    pub const X4C4: Pixman = Pixman(134479872);
    pub const X4G4: Pixman = Pixman(134545408);
    pub const A4: Pixman = Pixman(67190784);
    pub const R1G2B1: Pixman = Pixman(67240225);
    pub const B1G2R1: Pixman = Pixman(67305761);
    pub const A1R1G1B1: Pixman = Pixman(67244305);
    pub const A1B1G1R1: Pixman = Pixman(67309841);
    pub const C4: Pixman = Pixman(67371008);
    pub const G4: Pixman = Pixman(67436544);
    pub const A1: Pixman = Pixman(16846848);
    pub const G1: Pixman = Pixman(17104896);
    pub const YUY2: Pixman = Pixman(268828672);
    pub const YV12: Pixman = Pixman(201785344);
}

use derive_more::{Display, Error};
pub use drm_4cc::FourCC;
pub use pixman::Pixman;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Display)]
pub struct UnknownPixmanFormat;

/// Maps alpha-bearing RGB formats to their opaque "X" counterparts when compatible.
///
/// This helps avoid unintended composition with transparent guest content on DMABUF paths.
#[inline]
pub const fn sanitize_opaque_fourcc(fourcc: drm_4cc::FourCC) -> drm_4cc::FourCC {
    match fourcc {
        drm_4cc::ARGB4444 => drm_4cc::XRGB4444,
        drm_4cc::ABGR4444 => drm_4cc::XBGR4444,
        drm_4cc::RGBA4444 => drm_4cc::RGBX4444,
        drm_4cc::BGRA4444 => drm_4cc::BGRX4444,
        drm_4cc::ARGB1555 => drm_4cc::XRGB1555,
        drm_4cc::ABGR1555 => drm_4cc::XBGR1555,
        drm_4cc::RGBA5551 => drm_4cc::RGBX5551,
        drm_4cc::BGRA5551 => drm_4cc::BGRX5551,
        drm_4cc::ARGB8888 => drm_4cc::XRGB8888,
        drm_4cc::ABGR8888 => drm_4cc::XBGR8888,
        drm_4cc::RGBA8888 => drm_4cc::RGBX8888,
        drm_4cc::BGRA8888 => drm_4cc::BGRX8888,
        drm_4cc::ARGB2101010 => drm_4cc::XRGB2101010,
        drm_4cc::ABGR2101010 => drm_4cc::XBGR2101010,
        drm_4cc::RGBA1010102 => drm_4cc::RGBX1010102,
        drm_4cc::BGRA1010102 => drm_4cc::BGRX1010102,
        drm_4cc::ARGB16161616 => drm_4cc::XRGB16161616,
        drm_4cc::ABGR16161616 => drm_4cc::XBGR16161616,
        drm_4cc::ARGB16161616F => drm_4cc::XRGB16161616F,
        drm_4cc::ABGR16161616F => drm_4cc::XBGR16161616F,
        drm_4cc::AVUY8888 => drm_4cc::XVUY8888,
        _ => fourcc,
    }
}

impl TryFrom<Pixman> for drm_4cc::FourCC {
    type Error = UnknownPixmanFormat;

    #[inline]
    fn try_from(p: Pixman) -> Result<Self, Self::Error> {
        match p {
            pixman::A8R8G8B8 => Ok(drm_4cc::ARGB8888),
            pixman::A8B8G8R8 => Ok(drm_4cc::ABGR8888),
            pixman::B8G8R8A8 => Ok(drm_4cc::BGRA8888),
            pixman::R8G8B8A8 => Ok(drm_4cc::RGBA8888),
            pixman::X8R8G8B8 => Ok(drm_4cc::XRGB8888),
            pixman::X8B8G8R8 => Ok(drm_4cc::XBGR8888),
            pixman::B8G8R8X8 => Ok(drm_4cc::BGRX8888),
            pixman::R8G8B8X8 => Ok(drm_4cc::RGBX8888),
            pixman::R5G6B5 => Ok(drm_4cc::RGB565),
            pixman::B5G6R5 => Ok(drm_4cc::BGR565),
            pixman::A1R5G5B5 => Ok(drm_4cc::ARGB1555),
            pixman::X1R5G5B5 => Ok(drm_4cc::XRGB1555),
            pixman::A1B5G5R5 => Ok(drm_4cc::ABGR1555),
            pixman::X1B5G5R5 => Ok(drm_4cc::XBGR1555),
            pixman::A4R4G4B4 => Ok(drm_4cc::ARGB4444),
            pixman::X4R4G4B4 => Ok(drm_4cc::XRGB4444),
            pixman::A4B4G4R4 => Ok(drm_4cc::ABGR4444),
            pixman::X4B4G4R4 => Ok(drm_4cc::XBGR4444),
            pixman::R8G8B8 => Ok(drm_4cc::RGB888),
            pixman::B8G8R8 => Ok(drm_4cc::BGR888),
            pixman::A2R10G10B10 => Ok(drm_4cc::ARGB2101010),
            pixman::X2R10G10B10 => Ok(drm_4cc::XRGB2101010),
            pixman::A2B10G10R10 => Ok(drm_4cc::ABGR2101010),
            pixman::X2B10G10R10 => Ok(drm_4cc::XBGR2101010),
            pixman::YUY2 => Ok(drm_4cc::YUYV),
            pixman::YV12 => Ok(drm_4cc::YVU420),
            pixman::C8 => Ok(drm_4cc::C8),
            _ => Err(UnknownPixmanFormat),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{drm_4cc, pixman, *};

    /// 测试 1: 验证 fourcc_code 宏生成的字节序是否正确
    /// DRM FourCC 标准是 Little Endian (低位在前)
    /// 'A'(0x41), 'B'(0x42), 'C'(0x43), 'D'(0x44) 应该组合成 0x44434241
    #[test]
    fn test_fourcc_generation() {
        // 手动构造一个 FourCC
        let code = drm_4cc::fourcc_code(b'A', b'B', b'C', b'D');

        // 验证它底层的 u32 值
        // 在内存中顺序应该是: 41 42 43 44
        // 作为 u32 读取应该是: 0x44434241
        assert_eq!(u32::from(code), 0x44434241);

        // 验证一个现有的常量
        // ARGB8888 定义为 fourcc_code('A', 'R', '2', '4')
        // 'A'=0x41, 'R'=0x52, '2'=0x32, '4'=0x34 -> 0x34325241
        assert_eq!(u32::from(drm_4cc::ARGB8888), 0x34325241);
    }

    /// 测试 2: 验证 is_premultiplied 逻辑
    /// 包含 Alpha 的应为 true，忽略 Alpha (X) 或无 Alpha 的应为 false
    #[test]
    fn test_is_premultiplied() {
        // Case A: 标准的 Alpha 格式 -> True
        assert!(pixman::A8R8G8B8.is_premultiplied());
        assert!(pixman::A1R5G5B5.is_premultiplied());
        assert!(pixman::A8.is_premultiplied());

        // Case B: 忽略 Alpha (X) 的格式 -> False
        // 虽然内存布局和 A8R8G8B8 一样，但 X 代表该通道被忽略(通常视为不透明)
        assert!(!pixman::X8R8G8B8.is_premultiplied());
        assert!(!pixman::X1R5G5B5.is_premultiplied());

        // Case C: 无 Alpha 的格式 -> False
        assert!(!pixman::R5G6B5.is_premultiplied());
        assert!(!pixman::R8G8B8.is_premultiplied());
    }

    /// 测试 3: 验证 Pixman 到 DRM 的正确映射
    #[test]
    fn test_pixman_to_drm_conversion_success() {
        // 1. 测试最常用的 ARGB 32位
        // Pixman: A8R8G8B8 (Memory: B G R A) -> DRM: ARGB8888 (Memory: B G R A)
        let drm_fmt = drm_4cc::FourCC::try_from(pixman::A8R8G8B8);
        assert_eq!(drm_fmt, Ok(drm_4cc::ARGB8888));

        // 2. 测试 16位 565
        // Pixman: R5G6B5 -> DRM: RGB565
        let drm_fmt_16 = drm_4cc::FourCC::try_from(pixman::R5G6B5);
        assert_eq!(drm_fmt_16, Ok(drm_4cc::RGB565));

        // 3. 测试 YUV 格式
        let drm_yuv = drm_4cc::FourCC::try_from(pixman::YUY2);
        assert_eq!(drm_yuv, Ok(drm_4cc::YUYV));
    }

    /// 测试 4: 验证未支持格式的错误处理
    #[test]
    fn test_pixman_to_drm_conversion_failure() {
        // 选取一个在 match 列表中不存在的格式，例如 G1
        let result = drm_4cc::FourCC::try_from(pixman::G1);

        // 应该返回错误
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), UnknownPixmanFormat);
    }

    /// 测试 5: 验证 X 和 A 映射到了不同的 DRM 格式
    /// 这一点很重要，防止拷贝粘贴错误导致 X 格式被映射成 A 格式
    #[test]
    fn test_x_vs_a_mapping() {
        let alpha = drm_4cc::FourCC::try_from(pixman::A8R8G8B8).unwrap();
        let no_alpha = drm_4cc::FourCC::try_from(pixman::X8R8G8B8).unwrap();

        assert_ne!(alpha, no_alpha);
        assert_eq!(alpha, drm_4cc::ARGB8888);
        assert_eq!(no_alpha, drm_4cc::XRGB8888);
    }

    #[test]
    fn test_sanitize_opaque_fourcc_converts_alpha_formats() {
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::ARGB8888), drm_4cc::XRGB8888);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::ABGR8888), drm_4cc::XBGR8888);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::RGBA8888), drm_4cc::RGBX8888);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::BGRA8888), drm_4cc::BGRX8888);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::ARGB2101010), drm_4cc::XRGB2101010);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::ARGB16161616F), drm_4cc::XRGB16161616F);
    }

    #[test]
    fn test_sanitize_opaque_fourcc_keeps_opaque_formats() {
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::XRGB8888), drm_4cc::XRGB8888);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::XBGR2101010), drm_4cc::XBGR2101010);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::YUYV), drm_4cc::YUYV);
        assert_eq!(sanitize_opaque_fourcc(drm_4cc::RGB565), drm_4cc::RGB565);
    }
}
