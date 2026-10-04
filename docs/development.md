# 开发与本地存储

## 环境与命令

开发目标为 Windows。需要 Node.js、pnpm 12.8.1、Rust MSVC 工具链、Visual Studio C++ / Windows SDK 和 WebView2。

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

验证与发布：

```sh
pnpm build
pnpm test
cargo test --manifest-path src-tauri/Cargo.toml
pnpm tauri build --bundles nsis
```

可执行文件为 `src-tauri/target/release/wiztree-diff.exe`，NSIS 安装包位于 `src-tauri/target/release/bundle/nsis/`。构建安装包不会自动安装；提交的 pnpm / Cargo 锁文件固定依赖解析。

## 代码结构

- `src/`：React 界面、Tauri API 调用、类型和格式化；`src/components/` 包含目录树、导入、统计和 treemap。
- `src-tauri/src/`：流式 CSV 导入、SQLite 存储、路径比较、文件分类、全局 treemap 和命令接口。
- `src-tauri/tests/` 与 `src/components/*.test.tsx`：自包含回归测试，不依赖用户 CSV、截图或验收记录。
- `src-tauri/capabilities/`、`tauri.conf.json` 与 `icons/`：应用权限、构建配置和必需图标资源。

## 缓存与权限

索引位于 `%LOCALAPPDATA%/com.wiztreediff.desktop/comparisons`。只保留当前成功对比和正在生成的新任务；完成后 checkpoint、原子替换 schemaVersion / comparisonId 清单，再删除旧派生索引。重启恢复完成索引，不重读 CSV。

损坏、未完成或不兼容的缓存要求重新导入，不猜测结构。索引可能明显大于 CSV，建议使用可写 SSD 并预留至少 30 GiB 空间；存储错误显示缓存位置。

前端仅有 core 默认权限与系统文件选择权限，没有任意文件读取权限。数据库、匹配规则及聚合在 Rust。CSV 拖放通过 Tauri 原生 WebView 事件接收路径，并将物理像素位置换算为 CSS 坐标。

## 源码包

根目录 `wiztree-diff-source.zip` 用于发送源码。包含源码、正式回归测试、构建配置、锁文件、应用图标、README、AGENTS、CHANGELOG 和本目录文档。

不包含 `verification/`、截图、`csv-example/`、临时文件、Git 元数据、依赖目录、生成的 schema、索引数据库、可执行文件或其他构建产物。源 CSV 示例仍可在原工作目录保留，但不是源码包或测试的依赖。

解压后按上述命令安装依赖并构建；无需验收截图或历史测试记录。
