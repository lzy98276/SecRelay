# 图标字体

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
3. `cargo test -p gen-icons` —— 有一条测试会把「16 个图标名都能解析出来」钉住，
   名字对不上会直接失败，而不是等到界面显示成一堆方框。
