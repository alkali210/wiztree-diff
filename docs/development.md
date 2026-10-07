# 开发与运行时内存

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
pnpm tauri build --no-bundle
pnpm tauri build --bundles nsis
```

可执行文件为 `src-tauri/target/release/wiztree-diff.exe`，NSIS 安装包位于 `src-tauri/target/release/bundle/nsis/`。构建安装包不会自动安装；提交的 pnpm / Cargo 锁文件固定依赖解析。独立桌面验证使用 `pnpm tauri build --no-bundle`，确保嵌入正式前端；直接 `cargo build` 不替代 Tauri 的构建配置。

## 代码结构

- `src/`：React 界面、Tauri API 调用、类型和格式化；`src/components/` 包含目录树、导入、统计和 treemap。
- `src-tauri/src/import.rs`：可复用的流式 CSV 解析、规范化、字段校验和取消控制，不依赖存储实现。
- `src-tauri/src/store.rs`：紧凑 Builder、只读 Comparison、实际父关系、导出范围与聚合收尾。
- `diff.rs`、`file_categories.rs`、`file_extensions.rs`：内存分页查询、原始详情及文件路径统计。
- `global_treemap.rs`：当前请求的完整布局、绘图、精确命中和定位；`commands.rs`：任务发布与布局句柄生命周期。
- `src-tauri/tests/` 与 `src/components/*.test.tsx`：自包含回归测试，不依赖用户 CSV、截图或验收记录。
- `src-tauri/capabilities/`、`tauri.conf.json` 与 `icons/`：应用权限、构建配置和必需图标资源。

## 生命周期、内存与权限

应用不保存比较数据库、清单、CSV 路径、PNG 或几何文件。启动的当前比较与任务为空，前端两侧路径为空，不恢复缓存或自动读取 CSV。旧版本 `%LOCALAPPDATA%/com.wiztreediff.desktop/comparisons` 目录不读取、不迁移，也不在启动时自动删除；如需回收历史磁盘占用，由用户自行清理该旧缓存目录。WebView2 自身的运行环境数据不属于比较持久化缓存。

新 Builder 独立于 `Arc<Comparison>` 中的当前成功结果；完整解析、匹配、层级/导出范围校验和基础统计成功后，在短锁内原子发布。取消与发布使用同一个运行时锁裁决；失败或取消丢弃新构建，但保留旧树、详情和布局。查询仅在短锁内校验比较 ID、克隆 Arc，随后在锁外读取不可变数据；释放大对象也在锁外。

Rust 常驻路径并集、两侧成员、共享冷详情、连续父子区间和目录聚合。构建期路径字典、排序和 MFT 工作数据在完成后释放；不再增长的数组在发布前一次收紧容量。内存随输入记录、路径和详情增长，不宣称固定内存。重新导入时还需容纳旧成功比较与新 Builder；正在执行的旧查询可能短暂保活旧 Arc。容量检查及主要构建分配采用显式错误，不截断输入，也不回退到磁盘数据库。

treemap 不阻塞比较发布。`get_full_treemap` 只构建当前指标、模式和深度，返回 `comparisonId` 与 `layoutId`；命中和高亮必须针对该已生成布局，不隐式创建图。重型布局、栅格化和 PNG 编码串行并设置取消点；布局注册最多保留展示中的旧图与请求的新图。前端在新图实际绘制后释放旧句柄，迟到响应、卸载和过期请求也释放或取消。After 只临时保留 Before 的容器内容区参考，不预生成其他模式帧。

`release_treemap(comparisonId, layoutId)` 精确且幂等地释放非空句柄；空 `layoutId` 仅取消该比较正在排队或计算的请求。前端将旧请求的取消与下一次取图串行衔接，避免迟到的取消误伤新视图；不存在或已替换的比较不会影响当前图。

全局类别/扩展名在导入时累积；所选目录扩展名按真实同侧目录按需汇总，排序缓存按容量及字符串占用限制为 16 MiB。树、根、扩展名保持分页；WebView 的树缓存仍有界，不接收完整 Rust 树、几何或路径集合。具体数据口径见 [data-model.md](data-model.md)。

前端仅有 core 默认权限与系统文件选择权限，没有任意文件读取权限。路径匹配、精确数值、聚合与 CSV 读取均在 Rust。CSV 拖放通过 Tauri 原生 WebView 事件接收路径，并将物理像素位置换算为 CSS 坐标。

## 大 CSV 实测（内存版本）

使用 `example-csv/` 中两份 CSV，共 2,335,246 条记录、1,169,123 个并集节点；Windows x64、Release、SSD，源文件已被读取，以下是暖系统文件缓存的一轮结果，不承诺冷启动或其他机器的固定耗时。

| 测量路径 | 实测 |
| --- | ---: |
| 后端导入、diff、校验与基础统计完整就绪 | 4.645 秒 |
| 按需大小 / 之后完整布局及图像响应 | 0.540 秒 |
| 按需大小 / 差异完整布局及图像响应 | 0.047 秒 |
| 按需分配 / 之前完整布局及图像响应 | 0.328 秒 |
| 基础 arena、成员、节点、邻接与聚合数组容量统计 | 约 424 MiB |
| 后端探针峰值工作集 / 提交量 | 约 536 / 598 MiB |
| 真实桌面点击开始至比较结果就绪 | 4.787 秒 |
| 真实桌面点击开始至默认之后图像已实际绘制 | 5.717 秒 |
| 真实桌面主进程峰值工作集 / 提交量 | 约 602 / 616 MiB |

后端探针分别请求三个视图，不同时缓存；其时间包括对应布局、PNG 与响应编码，不是仅解析时间。真实桌面通过原生文件对话框手动选入同一输入，首帧以 Canvas 已绘制像素和完整图表统计观察，不把收到 JSON 当作完成绘图。两条路径的内存统计都不包含独立 WebView2 进程；数组容量统计也不等于整个进程的内存。替换另一份同等大小的成功比较会有额外旧数据峰值，本表不覆盖该情况。

本轮通过 Rust 73 项、前端 16 项消费行为回归，并在真实桌面验证空启动/空重启、手动选择、失败与取消保留旧结果、树展开、图表点击详情联动、指标/深度切换和大数据连续视图切换。移除数据库换来更少写入与更直接查询，但常驻内存高于过去主要留在磁盘的完成结果；不能把旧缓存版本不同布局、预生成范围的历史数字当作本轮同口径对照。

## 源码包

根目录 `wiztree-diff-source.zip` 用于发送源码。包含源码、正式回归测试、构建配置、锁文件、应用图标、README、AGENTS、CHANGELOG 和本目录文档。

不包含 `verification/`、截图、`csv-example/`、`example-csv/`、`.tmp/`、Git 元数据、依赖目录、生成的 schema、历史缓存数据库、可执行文件或其他构建产物。源 CSV 示例仍可在原工作目录保留，但不是源码包或测试的依赖。

解压后按上述命令安装依赖并构建；无需验收截图或历史测试记录。
