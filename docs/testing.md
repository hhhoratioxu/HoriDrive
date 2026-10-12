# 测试与构建记录

所有测试使用临时目录，不访问用户文件；测试证书只有公开部分写入测试目录，运行时私钥保存在 OS 凭据存储或测试进程内存。

## 已运行（Windows x64，2026-10-12）

- `npm run build`：TypeScript 检查与 Vite production build 通过。
- `cargo +stable-x86_64-pc-windows-gnu test -p horidrive-core -p horidrive-storage -p horidrive-transport`：8 项通过，1 项大文件测试当时忽略。
- CLI GNU Windows 构建通过，实际运行 `--help` 与公开设备身份创建通过；Credential Manager 可用。
- Rust MSVC 构建初次因缺少 `link.exe` 失败；Build Tools 自动安装返回 1602。本机改用已有 GNU 工具链继续验证，推荐发布工具链仍为 MSVC。

## 自动化覆盖

- 空 / 小 / 跨块文件；中文与特殊字符文件名；嵌套目录。
- 文件导入、复制、移动、删除、恢复、历史版本恢复、重启索引持久化。
- 相对路径、Windows 保留名称、ADS、内部目录访问、同根目录实例锁。
- BLAKE3 块不一致、CAS 配额不足、双节点加密传输与删除传播。
- 并发编辑保留两个副本；未授权证书被拒绝；撤销授权后的现有连接拒绝。

1 GiB+ 测试需显式执行 `cargo test -p horidrive-core -- --ignored`，会临时写入及导出约 1.004 GiB 的文件。

## 后续已运行验证

- 1.004 GiB 实际文件流式索引与导出测试通过。
- 真实两个独立 CLI 节点进程：公开证书配对、中文 / 特殊字符 / 空文件、并发修改、删除 tombstone、配额拒绝与重试、重启、远程列表 / 按需下载全部通过。
- 两节点在第一个 4 MiB 块已校验后被强制终止；重新启动只传剩余 8 MiB，12 MiB 文件完整恢复。上传配额失败重试也验证不重复发送已校验块。
- 安装 journal 恢复、损坏替换保留原文件、Windows junction 拦截、大小写别名拦截、历史与自动替换备份上限、主动删除保留、notify 真实写入监听通过。
- 取消发生在限速等待期间；远端不出现半成品文件；后续同步复用分块、重新发送流量为 0 的测试通过。
- mDNS 真实公告并解析到本机网络地址的显式测试通过（5 秒发现窗口）。它不代表已验收所有网络 / 防火墙条件。
- Rust default-members 与 Tauri desktop 的 Clippy `-D warnings` 均通过；TypeScript 与 Vite production build 通过。
- 实际 Tauri 窗口启动、本机真实容量读取、界面新建文件夹、系统文件选择框导入及磁盘内容核对通过。后续 UI 验收被 Windows 安全中心的测试进程网络提示挡住；代理没有操作该系统安全提示。
- 本机 GNU / debug Windows NSIS 安装包已实际生成，未签名；存在 GNU 链接器的 manifest 合并警告。本机 MSVC 缺失，正式 MSVC / macOS 编译由 GitHub Actions 工作流提供，只有实际完成后才记录通过。

## 尚未完成的验收

真实物理磁盘耗尽、跨 OS 权限拒绝注入、物理局域网 1 / 2.5 Gbps 基准、长时间后台运行、macOS 安装启动、移动端、断电级文件系统恢复。自动化的配额拒绝不等于真实磁盘耗尽；同机 QUIC 测试不等于真实跨网验证。
