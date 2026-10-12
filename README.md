# HoriDrive

**Your Cloud. Your Rules.**

开源、Local-first 的私人云盘。文件保存在自己的 Windows / Mac / NAS 磁盘上，不需要购买 VPS 或存储套餐。主节点离线时，只有已下载的本地文件可用。

## 当前版本：0.1.0 Preview

这是实际操作磁盘的开发预览版。包含 Rust Workspace、SQLite 索引、React / Tauri 2 桌面界面、CLI、后台接收节点及原创同步逻辑；不依赖 HoriLink 或其他云盘项目。

- 文件浏览、搜索、中文文件名、排序、新建文件夹、导入 / 导出、复制 / 移动 / 重命名、回收站恢复。
- 图片与 UTF-8 文本预览；文件历史版本与恢复，每个路径最多 20 个版本。
- BLAKE3 内容寻址的 4 MiB 分块；流式读写、分块去重、校验后安装文件。
- QUIC / TLS 1.3 双向设备认证；系统 Credential Manager / Keychain 保存私钥；明确交换公开证书并核对指纹。
- 手动 IP / 域名连接、已配置的 Tailscale 地址；双向同步、向量时钟、并发修改保留冲突副本、删除 tombstone。
- mDNS 局域网公告与发现（只提供未认证的地址提示，仍需双方证书授权）。
- SQLite 持久化传输记录，失败后重试复用已验证的块；notify 文件监听；CLI 可周期重连同步。
- 桌面与 CLI 浏览远程目录、按需下载单个文件；下载后可离线访问。
- 传输速度、按分块限速、协作取消，取消后可从已验证分块继续。

**尚未完成：** 多存储卷 / 多同步文件夹配置、原生 NAT 穿透、移动端、托盘 / 自启动、桌面自动重连、自适应并发、专业性能调优、签名安装包。桌面当前通过按钮同步；CLI `serve --interval` 可定期自动同步。没有宣称 1 / 2.5 Gbps 线速。

磁盘内容没有静态加密。QUIC 的传输加密不等于端到端静态加密。预览版仍应配合独立备份；已信任设备拥有整个选定空间的访问权限。

## 编译与启动

Windows 推荐 Rust Stable **MSVC**、Visual Studio C++ Build Tools / Windows SDK、Node.js 22+、系统 WebView2。详见 [Tauri 官方前置依赖](https://v2.tauri.app/start/prerequisites/)。macOS 需要 Xcode Command Line Tools。

```powershell
git clone https://github.com/hhhoratioxu/HoriDrive.git
cd HoriDrive
cargo build -p horidrive-cli
cd desktop
npm ci
npm run tauri dev
```

桌面默认空间为用户目录下 `HoriDrive Files`，可以通过 `HORIDRIVE_ROOT` 环境变量选择另一个独立目录。每个空间仅允许一个 HoriDrive 进程打开；桌面和 CLI 不应同时打开同一目录。

```powershell
# 编译安装包
cd desktop
npm run tauri build
# CLI：可指定任意独立数据目录
cargo run -p horidrive-cli -- --root D:/MyDrive info
cargo run -p horidrive-cli -- --root D:/MyDrive mkdir 资料
cargo run -p horidrive-cli -- --root D:/MyDrive import D:/example.txt 资料/example.txt
```

## 两台设备配对与真实传输

在各自设备上分别生成公开设备文件：

```powershell
horidrive --root D:/DriveA identity device-a.json
horidrive --root D:/DriveB identity device-b.json
```

安全交换 JSON 文件；输出的完整 64 字符证书指纹必须通过独立可靠渠道核对。JSON 仅含公开证书。**只确认文件本身附带的指纹无法防御中间人攻击。**

在 A 上添加 B，在 B 上添加 A（填写各自地址与对方指纹）：

```powershell
# 在 A 上
horidrive --root D:/DriveA trust device-b.json --fingerprint <B指纹> --address 192.168.1.20:44330 --name B
# 在 B 上
horidrive --root D:/DriveB trust device-a.json --fingerprint <A指纹> --address 192.168.1.10:44330 --name A
# 接收服务默认仅监听 loopback。局域网需要显式指定本机局域网地址。
horidrive --root D:/DriveB serve --listen 192.168.1.20:44330
# 在 A 上同步；传输 A 的本地文件并拉取 B 的文件。
horidrive --root D:/DriveA sync <B指纹>
```

两个设备需各自允许 UDP 44330 入站；程序不会修改防火墙。跨网可填写已配置的 Tailscale 地址。主节点必须在线且地址可达。授权变更后重新启动接收服务，`revoke` 会在现有连接的下一次请求时拒绝已撤销设备。

持续运行时可在独立后台节点上使用 `serve --interval 15` 每 15 秒尝试同步已信任设备，失败记录写入本地数据库，下次自动重试。接收节点与同步根目录生命周期一致。

## 安全与同步策略

远端路径只能是选定根目录下的相对路径；拒绝路径遍历、Windows ADS / 保留名称和符号链接 / reparse point。同步使用向量时钟判断因果关系，不以修改时间覆盖文件。

并发不同内容采用确定性规则：保留 live 内容优先于删除、文件优先于目录，文件内容哈希用于确定主副本；另一个文件保留为 `原名.conflict-哈希前缀`。整个文件和所有块都通过 BLAKE3 校验后写入 staging；被替换文件进入回收站，安装日志支持重启恢复。目录非空时不会直接套用远端目录删除。

SQLite 索引和块存储位于根目录内的 `.horidrive`，用户文件浏览和远端路径不能访问该目录。每个路径最多 20 个历史版本、20 个自动替换备份；主动删除的回收站内容不会自动清空。配额仅限制内容块，不能约束整个磁盘或回收站。完整威胁模型见 [SECURITY.md](SECURITY.md)。

## 开发与验证

```powershell
cargo fmt --all --check
cargo test
cargo test -p horidrive-core -- --ignored
cd desktop
npm ci
npm run build
```

测试说明：[docs/testing.md](docs/testing.md)。架构：[docs/architecture.md](docs/architecture.md)。贡献方式：[CONTRIBUTING.md](CONTRIBUTING.md)。实际测试与构建结果按阶段更新，不将未运行的 macOS 或硬件测试写成通过。
