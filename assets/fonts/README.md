# 字体

本目录放两套字体，用途完全不同：

| 目录/文件 | 用途 | 许可证 |
|---|---|---|
| `FluentSystemIcons-Resizable.ttf` | **图标**（导航、按钮、状态） | MIT（Microsoft） |
| `MiSans/` | **界面文字**（可切换的系统默认 / miSans） | MiSans 许可协议（小米） |

两者的许可文件都在同目录：`LICENSE-FluentSystemIcons`、[LICENSE-MiSans.md](LICENSE-MiSans.md)。

---

# 一、图标字体：Fluent System Icons

## 来源

- **Fluent System Icons** — https://github.com/microsoft/fluentui-system-icons
- 文件：`FluentSystemIcons-Resizable.ttf`（可变字重版本）
- 许可证：**MIT**，Copyright (c) Microsoft Corporation
- 完整许可证见同目录的 [LICENSE-FluentSystemIcons](LICENSE-FluentSystemIcons)

MIT 与主仓库的 GPL-3.0-or-later 兼容，可以随二进制一起分发。

## 这个字体有两个必须知道的坑

### 1. 没有连字，只能用码位

`GSUB` 表只有几十字节，**不含连字特性**。所以不能像某些图标字体那样写
`Text { text: "home"; }` 来显示图标 —— 必须用 **Private Use Area 码位**。

码位由生成器从字体里抽出来，不要手抄：

```bash
cargo run -p gen-icons
```

它会读本目录的字体，生成 `apps/secrelay-desktop/ui/icons.slint`。

### 2. 尺寸是 20，不是 24

这是 `Resizable`（可变字体）版本，**只保留一套基准尺寸 20**，靠变体轴缩放到其它尺寸。
所以你找不到 `ic_fluent_desktop_24_regular` 这个名字 —— 只有 `_20_`。
另外部分图标名与常规版本不同（例如没有 `screen_share`，用的是 `share_screen_start`）。

名字写错时生成器会直接列出候选字形，不用靠猜：

```
以下字形在字体里找不到（名称可能随字体版本变化）：
  screen -> ic_fluent_screen_share_20_regular

按关键词查找可用字形名：
  share_screen: ic_fluent_share_screen_start_20_regular, ...
```

## 换字体版本时

1. 替换本目录的 `.ttf`；
2. `cargo run -p gen-icons`；
3. `cargo test -p gen-icons` —— 有一条测试会把「图标名都能解析出来」钉住，
   名字对不上会直接失败，而不是等到界面显示成一堆方框。

---

# 二、界面字体：MiSans

## 来源与许可

- **MiSans** — 小米，https://hyperos.mi.com/font/
- 许可证：**免费商用**，但**要求在软件中注明使用了 MiSans**
- 完整说明见 [LICENSE-MiSans.md](LICENSE-MiSans.md)

⚠️ **这个署名是许可义务，不是可选装饰。** 它已经做进应用里：
设置窗口 →「关于」→「字体」。改那段文案前请先读 LICENSE-MiSans.md。

## 只放两个字重

```
MiSans/MiSans-Regular.ttf    （7.75 MB）
MiSans/MiSans-Demibold.ttf   （7.69 MB）
```

上游一共 10 个字重（合计约 77 MB）。这里只放 UI 实际用到的两个 —— 常规正文与
标题用的半粗体。要加字重就从上游拷对应文件，然后在 `FontChoice::families`
（`crates/secrelay-theme/src/font.rs`）里接上。

## 最大的坑：Demibold 是**另一个字体族**

这两个文件的内部 family 名**不相同**：

| 文件 | family 名 |
|---|---|
| `MiSans-Regular.ttf` | `MiSans` |
| `MiSans-Demibold.ttf` | `MiSans Demibold` |

所以**不能靠 `font-weight: 600` 让渲染器挑到 Demibold** —— 它只认 family 名。
Slint 那边因此有两个属性：`Theme.ui-font` 与 `Theme.ui-font-bold`，
粗体文本要同时写 `font-family: Theme.ui-font-bold`。

系统字体则相反（同族内有 Bold 权重），所以 `FontChoice::System` 返回的两个名字
是一样的，由 `font-weight` 生效。这个差异有测试钉着
（`crates/secrelay-theme/src/font.rs` 的 `miSans_的粗体是另一个字体族`）。

## 体积提醒

两套字体加起来约 17 MB，都会在编译期嵌进二进制。这是"不依赖系统是否装字体"
的代价；如果哪天在意体积，可以对 miSans 做子集化（但**不得改字形外观**，
见许可协议）。

