//! SecRelay 媒体层：采集与帧表示。
//!
//! 这一层只做一件事：**把各平台的采集 API 归一成"一串 BGRA 帧"**。
//! 编解码、脏矩形差分、渲染、传输都不在这里。
//!
//! # 模块分类
//!
//! | 模块 | 职责 | 是否依赖平台 |
//! |---|---|---|
//! | [`frame`] | 帧的数据表示、差分、指纹 | ❌ 纯逻辑（可编译到 WASM） |
//! | [`capture`] | `ScreenSource` 接口与错误类型 | ❌ 纯接口 |
//! | [`synthetic`] | 合成画面源（测试与无显示器环境） | ❌ 纯逻辑 |
//! | [`backend`] | 各平台真实后端（Windows DXGI / Linux PipeWire / macOS SCK …） | ✅ 按 `cfg(target_os)` 编译 |
//!
//! 这样切分的意义：**上层只认 [`ScreenSource`] trait 与 [`VideoFrame`]**，
//! 加一个平台不需要改任何既有代码，也不需要动 UI。

pub mod backend;
pub mod capture;
pub mod convert;
pub mod frame;
pub mod png;
pub mod synthetic;

pub use backend::{native_backend_name, open_default_source, HAS_NATIVE_BACKEND};
pub use capture::{CaptureError, ScreenSource};
pub use convert::{scale_for_width, to_rgba_scaled, ConvertError, RgbaImage};
pub use frame::{expected_len, PixelFormat, VideoFrame};
pub use synthetic::SyntheticScreenSource;

#[cfg(target_os = "windows")]
pub use backend::windows_dxgi::DxgiScreenSource;
