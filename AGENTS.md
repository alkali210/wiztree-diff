# 项目协作约定

## 项目与布局

WizTree Diff 是 Windows 本地 WizTree CSV 对比应用，前端为 React/TypeScript，桌面与数据处理为 Tauri 2 / Rust / SQLite。

- `src/`：界面、类型、格式化及 IPC 调用；`src/components/`：主要交互组件与前端回归测试。
- `src-tauri/src/`：导入、存储、比较、统计、treemap 及命令接口。
- `src-tauri/tests/`：Rust 集成测试；测试输入应自包含。
- `docs/`：详细使用说明、数据契约和开发说明。README 只保留概述、使用及必要命令。

## 实现约束

- 先阅读相关代码，沿用现有结构；优先正确、简单、可维护的实现，不引入重复通路或兼容别名。
- CSV 只读，不访问导出路径所指向的原文件。导入保持流式、分批写库，不将全部文件树或路径集合搬入内存或 WebView。
- 路径匹配、大小计算和聚合在 Rust；字节、增减和 MFT 经 IPC 使用十进制字符串，前端使用 BigInt，不能转成不精确的 Number。
- 同步维护 `src-tauri/src/types.rs` 与 `src/types.ts` 的 IPC 契约，更新所有调用方。
- 分配值按文件路径保留，不按 MFT 去重；目录 aggregate 与文件路径汇总不得混用。具体规则见 `docs/data-model.md`。
- treemap 保持全局范围；差异基于之前布局，目录层级和选择高亮不能悄悄改变统计范围。
- 保持分页与有界缓存；异步响应必须防止旧比较或旧选择覆盖新状态。Tauri 拖放坐标需处理物理像素与 CSS 像素的差别。
- 修改持久化数据结构时同步处理 schema 版本及不兼容缓存；不要猜测旧数据库结构。
- 保持最小权限，不为前端添加任意文件读取权限。

## 命令与验证

使用 pnpm，保留 `pnpm-lock.yaml` 和 `src-tauri/Cargo.lock`，不要引入另一套包管理锁文件。

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm test
cargo test --manifest-path src-tauri/Cargo.toml
pnpm tauri dev
pnpm tauri build --bundles nsis
```

运行覆盖改动路径的测试。交互改动需在真实桌面窗口验证；不能只以编译成功代替行为验证。测试关注行为、边界、精度、状态变化和错误，不固定无关文案、实现细节或 mock 转发。

## 文档与交付

- 更新受影响的文档和 CHANGELOG；详细内容分类放入 `docs/`，避免不断扩充 README。
- 源码包使用根目录 `wiztree-diff-source.zip`，仅包含源码、正式测试、构建所需配置/锁文件/图标和必要文档。
- 排除 `verification/`、截图、`csv-example/`、`.tmp/`、Git 元数据、`node_modules/`、`dist/`、`src-tauri/target/`、`src-tauri/gen/`、数据库和临时记录。
- 打包后核对条目、解压并验证构建；删除自己的临时验证目录，不改动用户 CSV 或与任务无关的文件。
