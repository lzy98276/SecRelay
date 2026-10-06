use std::path::PathBuf;

fn main() {
    // 用 Slint 内置的 fluent 风格（Windows 的 Fluent Design 组件）。
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());

    slint_build::compile_with_config("ui/app.slint", config).expect("编译 Slint UI 失败");
    println!("cargo:rerun-if-changed=ui/app.slint");

    embed_app_icon();
}

/// 把 `assets/icons/secrelay.res` 交给链接器，让 exe 带上应用图标
/// （资源管理器、任务栏固定项、Alt-Tab 取的就是它）。
///
/// 这里**刻意不用** `winresource` / `embed-resource`：那两个 crate 在 MSVC 目标上都要 Windows SDK 的
/// `rc.exe`，开发机不一定装了、CI 更不一定。所以资源在仓库里预编译好（`secrelay.res`，由
/// `secrelay.rc` + `secrelay.ico` 生成），构建期只做"把它喂给链接器"这一件事 —— 不需要任何外部工具。
/// 重生成方式见 `assets/icons/README.md`。
///
/// 找不到资源时只打警告、不失败：exe 退回默认图标，功能不受影响。
fn embed_app_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let res = manifest_dir.join("../../assets/icons/secrelay.res");

    if !res.exists() {
        println!(
            "cargo:warning=找不到 {}，exe 将使用默认图标（重生成方式见 assets/icons/README.md）",
            res.display()
        );
        return;
    }

    println!("cargo:rerun-if-changed={}", res.display());
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
