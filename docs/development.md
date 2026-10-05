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

索引位于 `%LOCALAPPDATA%/com.wiztreediff.desktop/comparisons`。只保留当前成功对比和正在生成的新任务；完成后 checkpoint、刷盘、原子替换 schemaVersion / comparisonId 清单，再删除旧派生索引。重启恢复完成索引，不重读 CSV。

新任务的数据库在发布前是可丢弃的派生文件：批量写入阶段不写 journal，页缓存上限 256 MiB，mmap 上限 512 MiB；只有全部导入、校验、比较、全局统计和默认完整图成功后，才恢复 WAL / FULL 同步、checkpoint 并刷盘，然后发布清单。取消、错误及启动时发现的未发布任务都直接清除，不对半成品执行恢复猜测。更大的固定页缓存是耗时与内存之间的显式权衡，不把完整路径集合或文件树载入 Rust 或 WebView。

当前缓存 schemaVersion 为 **8**；旧版本需重新导入。损坏、未完成或不兼容的缓存要求重新导入，不猜测结构。索引大小取决于记录和已访问的图形/统计范围；建议使用可写 SSD，并为当前成功缓存及新任务预留空间。存储错误显示缓存位置。

前端仅有 core 默认权限与系统文件选择权限，没有任意文件读取权限。数据库、匹配规则及聚合在 Rust。CSV 拖放通过 Tauri 原生 WebView 事件接收路径，并将物理像素位置换算为 CSS 坐标。

## 大 CSV 实测口径

使用 `example-csv/` 两个大文件，共 2,335,246 条记录，Release 构建、SSD；耗时包含两个输入的导入、导出根/目录计数校验、比较、全部工作区统计、默认无限深度“之后 / 大小”图的完整几何和 PNG、刷盘发布及首次数据读取。原始实现完整处理实测 102.5–118.9 秒；修改后的真实 Tauri 发布窗口实测 **26.7 秒**，不是只计 CSV 解析。该值是本机测量，不保证不同硬件或冷缓存时固定达标。

默认完成缓存实测约 **1.59 GiB**，原始实现约 **2.05 GiB**。其他指标/模式/深度和目录统计首次访问时生成并缓存，因此后续占用会增长；新任务完成前还需容纳旧成功缓存。

修改后的应用主进程峰值工作集实测约 **463 MiB**，峰值提交内存约 **530 MiB**；完成后工作集约 **34 MiB**。此数字不包含独立 WebView2 进程。原始无桌面壳后台基准峰值工作集约 **105 MiB**，两者进程构成不同，不作为严格的 UI 内存同比；较大的固定缓存确实增加处理期内存，以换取更短处理时间。

## 源码包

根目录 `wiztree-diff-source.zip` 用于发送源码。包含源码、正式回归测试、构建配置、锁文件、应用图标、README、AGENTS、CHANGELOG 和本目录文档。

不包含 `verification/`、截图、`csv-example/`、临时文件、Git 元数据、依赖目录、生成的 schema、索引数据库、可执行文件或其他构建产物。源 CSV 示例仍可在原工作目录保留，但不是源码包或测试的依赖。

解压后按上述命令安装依赖并构建；无需验收截图或历史测试记录。
