# Oxipresso Editor Client

**中文** · [English](#english)

[Oxipresso](https://github.com/EliorFoy/oxipresso)（TeXpresso 式增量 TeX 预览服务器）的 **GUI 实时预览客户端**：拉起渲染服务器、驱动增量重排、把排版结果以匹配显示密度的清晰度实时渲染出来。编辑器与预览双向同步——打字时预览自动翻到你正在编辑的页面。

> 本项目是 [TeXpresso](https://github.com/let-def/texpresso)（Frédéric Bour，MIT）编辑前端的 Rust/Slint 重实现，与 Oxipresso 渲染服务器通过 TeXpresso 兼容的编辑器协议（stdio 上的 S-expression/JSON）通信。

## 特性

- **实时增量预览**：按键 → 增量重排 → 新页面以约 120ms 的交叉淡化就位；未变的页面纹丝不动；排版进行中的页面流式上屏（不等整篇排完）。
- **编辑 ↔ 预览同步**：正向 SyncTeX——输入爆发结束后约 350ms，预览自动翻到你正在编辑的源码行所在的页面。
- **清晰的渲染**：页面位图按 面板宽 × 缩放 × 系统 DPI 精确渲染（不做 96dpi 放大糊图）；经典 Type1 字体经 dvips map/.enc 编码向量正确解析（LM 数学字体、黑体 𝕊、花体 𝓜 全部正确）。
- **完整工作台**：左侧源码编辑（直接打字即推送增量）、右侧缩放平移预览、翻页/缩放工具栏、引擎日志面板、状态栏（页码/产物大小/渲染耗时）。

## 使用

1. 按 [Oxipresso](https://github.com/EliorFoy/oxipresso) 的说明构建渲染服务器 `oxipresso.exe`（需要真实引擎模式）。
2. 把 `oxipresso-editor-client.exe` 与 `oxipresso.exe` 放在同一目录。
3. 运行：

```bash
oxipresso-editor-client paper.tex
```

客户端会自动拉起 `oxipresso.exe`（也可用 `--binary PATH` 指定）、注册文档、驱动常驻会话，之后在左侧源码里打字即可看到右侧实时刷新并跟随翻页。

```
用法: oxipresso-editor-client [--binary PATH] [--json] <document.tex>
```

## 构建

前置条件：Rust、vcpkg（`freetype`，Windows 用 `x64-windows-static-md`）、TeX Live / TinyTeX（kpsewhich 在 PATH，供字体解析）。

```powershell
$env:VCPKG_ROOT = "F:\code\vcpkg"
cargo build --release
# 产物: target/release/oxipresso-editor-client.exe
```

## 工作原理

- 客户端拉起 `oxipresso.exe -stream`，在其 stdio 上讲 TeXpresso 编辑器协议（`register`/`open-base64`/`resume`/`change`）。
- 引擎每个增量 pass 后写出 XDV 产物与 SyncTeX sidecar；渲染线程按 面板×缩放×DPI 密度渲染当前页，未变位图直接跳过，页数指示器在排版中保持稳定。
- 编辑位置（与上次内容的首差异，按 UTF-8 字符边界对齐）经正向 SyncTeX 映射为页码，防抖 ~350ms 后自动翻页。

## 与原项目的关系

[TeXpresso](https://github.com/let-def/texpresso) 的编辑器前端（emacs/vscode 插件）展示了编辑与渲染如何通过 wire 协议协作。本客户端把同样的协作带到一个独立的 Slint GUI 里。感谢 Frédéric Bour 与 TeXpresso 的贡献者们。

## 许可证

[MIT](LICENSE)

---

<a id="english"></a>

# Oxipresso Editor Client (English)

A **GUI live-preview client** for [Oxipresso](https://github.com/EliorFoy/oxipresso), the TeXpresso-style incremental TeX preview server: it launches the render server, drives incremental re-typesetting, and renders the result crisply at the display's native density. The editor and the preview stay in sync — while you type, the preview automatically turns to the page you are editing.

> This is a Rust/Slint reimplementation of TeXpresso's editor frontend, talking to the Oxipresso render server over the TeXpresso-compatible editor protocol (S-expressions/JSON on stdio).

## Features

- **Live incremental preview**: keypress → incremental re-typeset → the new page fades in over ~120ms; unchanged pages never repaint; pages stream in while the pass is still running.
- **Editor ↔ preview sync**: forward SyncTeX — ~350ms after a typing burst settles, the preview flips to the page containing your source line.
- **Crisp rendering**: the page bitmap is rendered at exactly pane-width × zoom × DPI scale (no blurry 96-dpi upscaling); classic Type1 fonts resolve through dvips map/.enc vectors (LM math fonts, blackboard 𝕊, calligraphic 𝓜 all correct).
- **Full workbench**: a source editor on the left (typing pushes increments), a zoomable pannable preview on the right, page navigation and zoom, an engine log panel, and a status bar (page / artifact size / render latency).

## Usage

1. Build the render server `oxipresso.exe` per [Oxipresso](https://github.com/EliorFoy/oxipresso) (real-engine mode required).
2. Place `oxipresso-editor-client.exe` next to `oxipresso.exe`.
3. Run:

```bash
oxipresso-editor-client paper.tex
```

The client launches `oxipresso.exe` (or use `--binary PATH`), registers the document, drives the resident session — then type in the left pane and watch the right pane refresh and follow your page.

```
usage: oxipresso-editor-client [--binary PATH] [--json] <document.tex>
```

## Build

Prerequisites: Rust, vcpkg (`freetype`; `x64-windows-static-md` on Windows), TeX Live / TinyTeX on PATH (font resolution).

```powershell
$env:VCPKG_ROOT = "F:\code\vcpkg"
cargo build --release
# output: target/release/oxipresso-editor-client.exe
```

## How it works

- The client spawns `oxipresso.exe -stream` and speaks the TeXpresso editor protocol on its stdio (`register` / `open-base64` / `resume` / `change`).
- After every incremental pass the engine writes the XDV artifact and the SyncTeX sidecar; the render thread rasterizes the current page at pane × zoom × DPI density, skips unchanged bitmaps, and keeps the page indicator stable during passes.
- The edit position (first difference against the last pushed content, snapped to a UTF-8 char boundary) maps through forward SyncTeX to a page; ~350ms after a burst settles the preview flips there.

## Relation to the original project

TeXpresso's editor integrations (emacs/vscode plugins) show how the editor and the renderer cooperate over the wire protocol. This client brings the same cooperation into a standalone Slint GUI. Thanks to Frédéric Bour and the TeXpresso contributors.

## License

[MIT](LICENSE)