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

1 GiB+ 测试需显式执行 `cargo test -p horidrive-core -- --ignored`，会临时写入及导出约 1.004 GiB 的文件。实际结果在运行后追加。

## 尚未完成的验收

真实物理磁盘耗尽、跨 OS 权限拒绝注入、物理局域网 1 / 2.5 Gbps 基准、长时间后台运行、macOS 安装启动、移动端、断电级文件系统恢复。自动化的配额拒绝不等于真实磁盘耗尽；同机 QUIC 测试不等于真实跨网验证。
