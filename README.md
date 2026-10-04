# WizTree Diff

Windows 本地 WizTree CSV 快照对比工具，使用 Tauri 2、React/TypeScript 和 SQLite。

## 功能概述

- 对比新增、删除、大小变化及文件/目录替换，支持目录树过滤与原始字段详情。
- 全局层级 treemap 展示目录结构；差异模式以“之前”为底图，用边框提示变化。
- 左侧按文件大类统计，主面板按实际扩展名统计；支持逻辑大小与分配大小。
- CSV 拖放导入、后台处理、取消任务和本地索引恢复。

## 使用

1. 点击“之前 CSV”“之后 CSV”选择文件，或分别拖入一个 CSV；需要时可交换两侧。
2. 点击“开始比较”。若有导出范围警告，核对后确认继续。
3. 展开目录查看差异，点击树行或 treemap 查看详情；treemap 默认深度无限制（0）。

输入须为 UTF-8 WizTree 树导出，使用完整路径和原始字节，至少包含“文件名称、大小、分配”列。结果反映 CSV 记录差异，不等同于实际磁盘操作；分配图不按硬链接去重。

## 开发命令

需要 Node.js、pnpm 12.8.1、Rust MSVC、Visual Studio C++/Windows SDK 和 WebView2。

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
pnpm build
pnpm test
cargo test --manifest-path src-tauri/Cargo.toml
pnpm tauri build --bundles nsis
```

发布产物位于 `src-tauri/target/release/`，安装包位于其 `bundle/nsis/` 子目录。

## 详细说明

- [使用说明](docs/usage.md)
- [输入、比较与统计口径](docs/data-model.md)
- [开发、缓存与源码包](docs/development.md)
- [项目协作约定](AGENTS.md)
