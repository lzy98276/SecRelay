//! 采集后端：每个平台一个模块，都实现 [`crate::capture::ScreenSource`]。
//!
//! | 平台 | 模块 | 状态 |
//! |---|---|---|
//! | Windows | [`windows_dxgi`] | ✅ DXGI Desktop Duplication |
//! | Linux X11 | — | ⬜ XShm |
//! | Linux Wayland | — | ⬜ PipeWire + xdg-desktop-portal（需要门户授权） |
//! | macOS | — | ⬜ ScreenCaptureKit |
//! | Android | — | ⬜ MediaProjection（每次会话需重新授权） |
//!
//! 新增后端时：在这个目录加一个模块，实现 `ScreenSource`，并在 `mod.rs` 里
//! **按 `cfg(target_os)` 重导出**。上层代码只认 trait，不认平台。

#[cfg(target_os = "windows")]
pub mod windows_dxgi;

#[cfg(target_os = "windows")]
pub use windows_dxgi::DxgiScreenSource;

/// 当前平台是否已有真实采集后端。
///
/// UI 用它决定要不要把"共享屏幕"按钮置灰并显示原因，而不是等用户点了才报错。
pub const HAS_NATIVE_BACKEND: bool = cfg!(target_os = "windows");

/// 当前平台后端名称（用于 UI 展示与日志）。没有后端时返回 `None`。
pub fn native_backend_name() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        Some("DXGI Desktop Duplication")
    } else {
        None
    }
}

/// 创建当前平台的默认采集源。
///
/// 没有后端时返回 [`crate::CaptureError::Unsupported`]，附带清晰的说明 ——
/// 而不是让调用方自己猜。
pub fn open_default_source() -> Result<Box<dyn crate::capture::ScreenSource>, crate::CaptureError> {
    #[cfg(target_os = "windows")]
    {
        return Ok(Box::new(DxgiScreenSource::new(0)?));
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err(crate::CaptureError::Unsupported(format!(
            "{} 平台的采集后端尚未实现（当前只有 Windows DXGI 与合成源）",
            std::env::consts::OS
        )))
    }
}
