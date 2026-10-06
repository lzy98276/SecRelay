//! 平台相关：**怎么把系统强调色取出来**。
//!
//! 解析逻辑全在父模块（纯函数、各平台都测得到）；这里只负责取到那段原始文本。
//! 每个取法都可能失败（键不存在、命令不存在、桌面环境不同），失败就返回 `None`
//! 让调用方继续试下一个来源。

use crate::Rgb;

/// 按平台顺序探测。返回 `(颜色, 来源名称)`；全部失败返回 `None`。
pub fn detect() -> Option<(Rgb, &'static str)> {
    #[cfg(target_os = "windows")]
    return windows_accent();

    #[cfg(target_os = "macos")]
    return macos_accent();

    #[cfg(all(unix, not(target_os = "macos")))]
    return linux_accent();

    #[allow(unreachable_code)]
    None
}

// ─────────────────────────────────────────────── Windows

/// Windows：读注册表里的强调色。
///
/// 先试 DWM 的 `AccentColor`（用户实际选的强调色），再退回资源管理器的 `AccentColorMenu`。
/// 两者的 DWORD 都是 `0xAABBGGRR`。
#[cfg(target_os = "windows")]
fn windows_accent() -> Option<(Rgb, &'static str)> {
    const SOURCES: [(&str, &str, &str); 2] = [
        (
            r"HKCU\Software\Microsoft\Windows\DWM",
            "AccentColor",
            "Windows 强调色",
        ),
        (
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
            "AccentColorMenu",
            "Windows 强调色（资源管理器）",
        ),
    ];

    for (key, value, label) in SOURCES {
        let Some(text) = reg_query(key, value) else {
            continue;
        };
        let Some(color) = crate::parse_windows_accent_output(&text) else {
            continue;
        };
        // 有些机器上键存在但为全黑（表示"未设置"），这种不算有效来源。
        if color != Rgb::new(0, 0, 0) {
            return Some((color, label));
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn reg_query(key: &str, value: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    /// 不弹控制台窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = std::process::Command::new("reg")
        .args(["query", key, "/v", value])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ─────────────────────────────────────────────── macOS

/// macOS：`AppleAccentColor` 是个索引，不是颜色，所以要查表。
///
/// 键不存在表示用户用默认外观（蓝），这仍然是一个有效来源 ——
/// 需求是"取系统主题色"，默认外观也是系统主题色的一部分。
#[cfg(target_os = "macos")]
fn macos_accent() -> Option<(Rgb, &'static str)> {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleAccentColor"])
        .output()
        .ok()?;

    if !output.status.success() {
        return Some((crate::accent_from_macos_index(-1), "macOS 默认强调色"));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let index: i32 = text.trim().parse().ok()?;
    Some((crate::accent_from_macos_index(index), "macOS 强调色"))
}

// ─────────────────────────────────────────────── Linux

/// Linux：桌面环境太多，按覆盖面从宽到窄依次尝试。
///
/// 没有 freedesktop 的统一"强调色"接口（portal 的 `accent-color` 键也没有普遍实现），
/// 所以这里退化为读各家配置。
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_accent() -> Option<(Rgb, &'static str)> {
    // GNOME 47+
    if let Some(text) = run(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "accent-color"],
    ) {
        if let Some(color) = crate::accent_from_name(&text) {
            return Some((color, "GNOME 强调色"));
        }
    }

    // KDE Plasma
    for tool in ["kreadconfig6", "kreadconfig5"] {
        if let Some(text) = run(
            tool,
            &["--file", "kdeglobals", "--group", "General", "--key", "AccentColor"],
        ) {
            if let Some(color) = crate::parse_kde_accent(&text) {
                return Some((color, "KDE 强调色"));
            }
        }
    }

    // 兜底：从 GTK 主题样式表里找选中态背景色
    for relative in [".config/gtk-4.0/gtk.css", ".config/gtk-3.0/gtk.css"] {
        if let Some(home) = std::env::var_os("HOME") {
            let path = std::path::Path::new(&home).join(relative);
            if let Ok(css) = std::fs::read_to_string(&path) {
                if let Some(color) = crate::parse_gtk_accent(&css) {
                    return Some((color, "GTK 主题色"));
                }
            }
        }
    }

    None
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    /// 探测函数在**任何**平台上都必须能正常返回（要么有颜色，要么 None），不能 panic。
    #[test]
    fn 探测不_panic() {
        let _ = super::detect();
    }
}
