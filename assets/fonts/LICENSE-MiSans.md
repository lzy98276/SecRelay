# MiSans 字体许可说明

## 来源

- **MiSans** — 小米（Xiaomi），与 Monotype、汉仪合作制作
- 官方页面：https://hyperos.mi.com/font/
- 官方常见问题：https://hyperos.mi.com/font/en/faq/
- 完整协议（PDF）：[MiSans 字体知识产权许可协议](https://hyperos.mi.com/font-download/MiSans%E5%AD%97%E4%BD%93%E7%9F%A5%E8%AF%86%E4%BA%A7%E6%9D%83%E8%AE%B8%E5%8F%AF%E5%8D%8F%E8%AE%AE.pdf)

本目录下的 `MiSans-Regular.ttf` 与 `MiSans-Demibold.ttf` 取自用户提供的
`SecRandom-C/SecRandom/Assets/Fonts/MiSans/`。

## 关键条款（摘自官方 FAQ）

官方 FAQ 对三条与本项目直接相关的问题的回答：

| 问题 | 官方回答 | 对本项目的影响 |
|---|---|---|
| 使用 MiSans 需要付费吗？ | **不需要。全部字体免费商用，可用于任何平台或商业项目。** | 可以随应用分发 |
| 可以嵌入字体吗？ | **可以。但你应当在软件中特别注明使用了 MiSans。** | **这是硬性条件**，见下 |
| 可以修改或重造字体吗？ | 可以调整字重、字距等；**但不得改变 MiSans 的外观或其组成部分。** | 只做子集化可以，改字形不行 |

> 原文（FAQ）：
> "No. All of MiSans Global's fonts are global free commercial-use fonts.
> This means you can use them on any platform or for any business project."
>
> "Can I use them as embedded fonts? Yes. However, you should specifically note
> in the software that MiSans was used."

## 本项目如何满足"在软件中注明"

**不是在 README 里注明，而是在应用界面里。** 许可要求的是"in the software"。

设置窗口 → **关于** → 「字体」区块列出：

- miSans —— 界面字体，由小米提供，可免费商用
- Fluent System Icons —— 图标，© Microsoft，MIT 许可

对应文案见 `crates/secrelay-i18n/src/lib.rs` 的 `Key::AboutMiSans` 与
`Key::AboutFluentIcons`，界面见 `apps/secrelay-desktop/ui/app.slint` 的「关于」分区。

**改动这里的署名文案时请先读本文件** —— 它承担的是许可义务，不是装饰。

## 与主仓库许可证的关系

SecRelay 主仓库是 GPL-3.0-or-later。MiSans 是**独立的字体作品**，不是代码，
所以两者的许可证不冲突：字体按 MiSans 自己的协议分发，代码按 GPL 分发。
分发时必须**同时**带上本文件与 `LICENSE-FluentSystemIcons`。
