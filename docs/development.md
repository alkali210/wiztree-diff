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

新任务的数据库在发布前是可丢弃的派生文件：批量写入阶段不写 journal，主库页缓存上限 256 MiB、临时工作索引页缓存上限 32 MiB、mmap 上限 512 MiB。整数排序索引按节点顺序读写，避免小型临时索引因随机读取反复换页。只有全部导入、校验、比较、全局统计和默认完整三模式成功后，才恢复 WAL / FULL 同步、checkpoint 并刷盘，再发布清单。取消、错误及启动时发现的未发布任务都直接清除，不对半成品执行恢复猜测，不把完整路径集合或文件树载入 Rust / WebView。按需指标、深度或目录统计提交后同样 checkpoint / truncate WAL，避免保留额外日志副本；已准备的模式切换不会进入写入路径。

当前缓存 schemaVersion 为 **11**；旧版本需重新导入，以切换到独立差异几何、绝对变化权重、PNG、标签及计数。路径前缀、快照成员、原始详情及紧凑比较数据分别存储，避免逐文件复制长路径和两侧相同详情。treemap 仍使用有界二进制块；之前/之后可复用未变目录的局部几何，差异不再共享之前几何。默认大小指标的三个模式在导入完成前准备；分配指标和额外深度首次使用时按三模式准备，不后台预生成未访问指标。同指标、同深度的模式切换不重新计算几何；按需写入的页缓存上限 64 MiB、mmap 上限 512 MiB。损坏、未完成或不兼容缓存要求重新导入，不猜测结构。新任务完成前仍需容纳旧成功缓存；存储错误显示缓存位置。

前端仅有 core 默认权限与系统文件选择权限，没有任意文件读取权限。数据库、匹配规则及聚合在 Rust。CSV 拖放通过 Tauri 原生 WebView 事件接收路径，并将物理像素位置换算为 CSS 坐标。

## 大 CSV 历史实测（缓存版本 9）

以下为已确认的性能优化提交 `ba9c499` 的历史口径；不将旧布局的耗时、占用数字当作版本 11 独立差异布局的新增测量结果。

使用 `example-csv/` 两个大文件，共 2,335,246 条记录，Release 构建、SSD。本轮真实 Tauri 窗口完整处理与首次接口读取实测 **26.5 秒**，包含两份输入、导出根/目录计数校验、精确路径比较、全部工作区统计、默认无限深度大小指标的三个完整模式、刷盘发布；流式导入两份输入本身约 **8.7 秒**。上一轮约 26.7 秒，但只准备默认之后图，其他模式首次查看另需布局。本轮仍未达到 WizTree 单快照导入到展示约 5 秒的水平；不将解析计时冒充完整展示时间，也不保证不同硬件或冷缓存时固定达标。

默认完成缓存实测 **931,733,504 字节（约 0.87 GiB）**，相比上一轮约 1.59 GiB 下降约 45%。首次启用分配指标额外准备约 **7.3 秒**，完整准备后数据库约 **0.99 GiB**。同一指标、同一深度的模式切换后台图像读取约 **2–5 毫秒**，真实窗口重新绘制约 **0.1 秒**，不全量扫描或写入几何。已准备模式的连续读取实测 0 次写入、缓存大小不变；冷读仍需读取 PNG，并非宣称物理磁盘读取为零。其他深度和目录统计按需生成、占用会增长；新任务完成前仍需容纳旧成功缓存。

本轮应用主进程峰值工作集实测约 **436 MiB**，峰值提交内存约 **515 MiB**，完成后工作集约 **35 MiB**，不包含独立 WebView2 进程。上一轮主进程峰值工作集约 463 MiB，峰值提交内存约 530 MiB。保持固定有界缓存，不以将完整树或路径集合载入 Rust / WebView 换取更短时间。

## 源码包

根目录 `wiztree-diff-source.zip` 用于发送源码。包含源码、正式回归测试、构建配置、锁文件、应用图标、README、AGENTS、CHANGELOG 和本目录文档。

不包含 `verification/`、截图、`csv-example/`、临时文件、Git 元数据、依赖目录、生成的 schema、索引数据库、可执行文件或其他构建产物。源 CSV 示例仍可在原工作目录保留，但不是源码包或测试的依赖。

解压后按上述命令安装依赖并构建；无需验收截图或历史测试记录。
