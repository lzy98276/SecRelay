//! Windows 屏幕采集：DXGI Desktop Duplication。
//!
//! 为什么选 DXGI Desktop Duplication（DDA）而不是 Windows.Graphics.Capture（WGC）：
//!
//! - DDA 是**全屏**采集，延迟最低、帧率最高，正是远程桌面要的；
//! - WGC 更适合"让用户选一个窗口"，且系统会画一个黄色提示边框；
//! - DDA 的代价是**需要自己处理**：UAC 安全桌面抓不到、混合显卡机器上可能拿不到输出。
//!
//! # 一个必踩的坑：复制表面不能直接 Map
//!
//! `AcquireNextFrame` 返回的 `IDXGIResource` **不能**直接 `IDXGISurface::Map` ——
//! 它是 GPU 上的复制表面，没有 CPU 访问权限，Map 会返回 `E_INVALIDARG (0x80070057)`。
//! 正确做法是先 `CopyResource` 到一张 `D3D11_USAGE_STAGING` + `D3D11_CPU_ACCESS_READ`
//! 的纹理，再 Map 那张。
//!
//! 本模块就是这么做的（[`DxgiScreenSource::staging`] 在构造时创建一次、复用）。
//!
//! # 已知限制（需求分析 §4 已记录）
//!
//! - **UAC 提权窗口 / Ctrl+Alt+Del 安全桌面抓不到**（那需要系统服务 + 特殊权限）；
//! - 全屏独占的 DirectX 游戏可能抓不到；
//! - 远程桌面（RDP）会话里通常返回 `DXGI_ERROR_UNSUPPORTED`。
//!
//! # 性能说明
//!
//! 「GPU 复制 + CPU 映射 + 逐行压平」是**两次拷贝**。这在 M0 探针阶段够用，
//! 但真正的产品路径应该把 D3D11 纹理直接交给硬件编码器（零拷贝），
//! 而不是每帧落一份 CPU 缓冲。这一步的收益与代价需要单独量测后再决定。

use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    IDXGISurface, DXGI_ERROR_WAIT_TIMEOUT, DXGI_MAP_READ, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::capture::{CaptureError, ScreenSource};
use crate::frame::{PixelFormat, VideoFrame};

/// 单次 `AcquireNextFrame` 的等待上限（毫秒）。真实的等待总时长由调用方给的 timeout 决定。
const MAX_ACQUIRE_WAIT_MS: u32 = 200;

/// DXGI Desktop Duplication 采集源。
pub struct DxgiScreenSource {
    /// 保持 device / context 存活：duplication 与 staging 纹理都依赖它们。
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// CPU 可读的暂存纹理。复制表面的 Map 会失败，必须先拷到这里。
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    sequence: u64,
}

impl std::fmt::Debug for DxgiScreenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DxgiScreenSource")
            .field("size", &format!("{}x{}", self.width, self.height))
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl DxgiScreenSource {
    /// 打开指定序号的显示器。
    ///
    /// 多显示器时每个 `IDXGIOutput` 对应一个显示器，需要各自建一个采集源。
    pub fn new(output_index: u32) -> Result<Self, CaptureError> {
        unsafe {
            // COM 初始化：该线程可能已经初始化过（返回 S_FALSE / RPC_E_CHANGED_MODE），
            // 都不影响 DXGI，因此忽略返回值。这里不配对调用 CoUninitialize ——
            // 采集源通常活到进程结束。
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .map_err(|e| CaptureError::Platform(format!("创建 DXGI 工厂失败：{e}")))?;

            let adapter = factory
                .EnumAdapters1(0)
                .map_err(|e| CaptureError::Platform(format!("枚举显卡适配器失败：{e}")))?;

            let output = adapter
                .EnumOutputs(output_index)
                .map_err(|e| CaptureError::Platform(format!("显示器 {output_index} 不可用：{e}")))?;

            let output1: IDXGIOutput1 = output
                .cast()
                .map_err(|e| CaptureError::Platform(format!("IDXGIOutput1 不可用：{e}")))?;

            // 建 D3D11 设备。必须带 BGRA 支持，否则 Desktop Duplication 拿不到输出。
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| CaptureError::Platform(format!("创建 D3D11 设备失败：{e}")))?;

            let device = device.ok_or_else(|| {
                CaptureError::Platform("D3D11 设备创建成功但没有返回设备对象".into())
            })?;
            let context = context.ok_or_else(|| {
                CaptureError::Platform("D3D11 设备创建成功但没有返回上下文".into())
            })?;

            let duplication = output1.DuplicateOutput(&device).map_err(|e| {
                CaptureError::Unsupported(format!(
                    "无法创建桌面复制接口：{e}。\
                     常见原因：当前会话不是本地控制台会话（如远程桌面 / 无头环境），\
                     或显卡驱动不支持 Desktop Duplication"
                ))
            })?;

            // windows 0.62 的 GetDesc 不接收出参，直接按值返回。
            let desc = duplication.GetDesc();
            let width = desc.ModeDesc.Width;
            let height = desc.ModeDesc.Height;

            // 关键：CPU 可读的暂存纹理，只建一次。
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| CaptureError::Platform(format!("创建暂存纹理失败：{e}")))?;
            let staging = staging.ok_or_else(|| {
                CaptureError::Platform("创建暂存纹理成功但没有返回纹理对象".into())
            })?;

            Ok(Self {
                _device: device,
                context,
                duplication,
                staging,
                width,
                height,
                sequence: 0,
            })
        }
    }

    /// 采集分辨率。
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 把暂存纹理拷成紧凑的 BGRA 缓冲。
    fn read_staging(&self) -> Result<Vec<u8>, CaptureError> {
        let surface: IDXGISurface = self
            .staging
            .cast()
            .map_err(|e| CaptureError::Platform(format!("暂存纹理不是 DXGI 表面：{e}")))?;

        let mut mapped = Default::default();
        unsafe {
            surface
                .Map(&mut mapped, DXGI_MAP_READ)
                .map_err(|e| CaptureError::Platform(format!("映射暂存纹理失败：{e}")))?;
        }

        let stride = self.width as usize * PixelFormat::Bgra8.bytes_per_pixel();
        let pitch = mapped.Pitch as usize;
        let mut data = vec![0u8; stride * self.height as usize];

        // 逐行拷贝：DXGI 的 Pitch 通常大于 stride，不能整块 memcpy。
        let copied = unsafe {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for y in 0..self.height as usize {
                    let src = mapped.pBits.add(y * pitch);
                    let row = std::slice::from_raw_parts(src, stride);
                    data[y * stride..(y + 1) * stride].copy_from_slice(row);
                }
            }))
        };

        // 无论拷贝是否失败都必须 Unmap，否则下次 Map 会失败。
        unsafe {
            let _ = surface.Unmap();
        }

        copied.map_err(|_| CaptureError::Platform("拷贝暂存纹理时越界".into()))?;
        Ok(data)
    }
}

impl ScreenSource for DxgiScreenSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() && timeout > Duration::ZERO {
                return Err(CaptureError::Timeout(timeout));
            }
            let wait_ms = remaining.as_millis().min(u128::from(MAX_ACQUIRE_WAIT_MS)) as u32;

            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            let acquired = unsafe {
                self.duplication
                    .AcquireNextFrame(wait_ms, &mut info, &mut resource)
            };

            match acquired {
                Ok(()) => {}
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    // 这段时间桌面没有变化 —— 正常现象，不是错误。
                    return Ok(None);
                }
                Err(err) => {
                    return Err(CaptureError::Platform(format!(
                        "AcquireNextFrame 失败：{err}"
                    )));
                }
            }

            // 只有鼠标移动时 LastPresentTime 为 0：画面内容没变，跳过。
            let has_image = info.LastPresentTime != 0;

            let frame = if has_image {
                let resource = resource.ok_or_else(|| {
                    CaptureError::Platform("AcquireNextFrame 成功但没有返回资源".into())
                })?;

                // 复制表面 → CPU 可读的暂存纹理 → Map。
                let source_texture: ID3D11Texture2D = resource
                    .cast()
                    .map_err(|e| CaptureError::Platform(format!("复制资源不是 2D 纹理：{e}")))?;
                let source_resource: ID3D11Resource = source_texture
                    .cast()
                    .map_err(|e| CaptureError::Platform(format!("纹理转资源失败：{e}")))?;
                let target_resource: ID3D11Resource = self
                    .staging
                    .cast()
                    .map_err(|e| CaptureError::Platform(format!("暂存纹理转资源失败：{e}")))?;

                unsafe {
                    self.context
                        .CopyResource(&target_resource, &source_resource);
                }

                let data = self.read_staging()?;
                let sequence = self.sequence;
                self.sequence += 1;
                Some(
                    VideoFrame::new(
                        self.width,
                        self.height,
                        PixelFormat::Bgra8,
                        data,
                        Instant::now(),
                        sequence,
                    )
                    .expect("宽高与缓冲区由采集层保证一致"),
                )
            } else {
                None
            };

            // 必须释放，否则后续 AcquireNextFrame 会一直失败。
            unsafe {
                let _ = self.duplication.ReleaseFrame();
            }

            if frame.is_some() {
                return Ok(frame);
            }
            // 仅指针更新：继续等，直到超时。
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn description(&self) -> String {
        format!(
            "Windows 桌面复制 {}x{}（DXGI Desktop Duplication）",
            self.width, self.height
        )
    }
}
