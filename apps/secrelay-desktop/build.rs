fn main() {
    // 把 ui/app.slint 编译成 Rust 代码；失败时给出清晰的构建错误。
    slint_build::compile("ui/app.slint").expect("编译 Slint UI 失败");
    println!("cargo:rerun-if-changed=ui/app.slint");
}
