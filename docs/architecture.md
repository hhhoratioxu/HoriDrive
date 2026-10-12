# 架构

HoriDrive 是独立原创项目，没有封装现有同步云盘。

| 模块 | 职责 |
| --- | --- |
| horidrive-storage | SQLite WAL / FULL、索引、版本、trash 意图记录、传输 jobs、不可变内容块、配额 |
| horidrive-core | 根目录访问、文件操作、完整性安装、向量时钟与冲突合并 |
| horidrive-transport | 公开设备身份、系统凭据、rustls mTLS、QUIC RPC、块拉取推送、双向同步 |
| horidrive-daemon | notify 去抖监听、接收生命周期、周期重连 |
| horidrive-cli | 可独立运行的管理工具与后台节点入口 |
| horidrive-desktop | Tauri commands、系统文件选择框、React 8 页面真实 UI |

一个根目录包含用户工作文件以及内部 `.horidrive/{metadata.sqlite3,blocks,trash,staging}`。OS 文件锁阻止同一根目录被多个节点实例同时打开。不同根目录可在同一台机器上启动多个节点。

每个 path 的索引条目包含类型、内容哈希、块序列、大小、修改时间与向量时钟。扫描时文件内容变化增加本机时钟；删除保存 tombstone。双向同步先获取 manifest，根据因果关系拉取缺失块、合并条目，再推送最终本地 revision。崩溃后已验证 CAS 块仍可复用。

SQLite 保存 `pending_install` 后才归档既有工作文件并安装 staging。启动时先恢复安装再扫描；独立改变的目标不会被盲目覆盖。替换文件保留在回收站。历史版本引用块，垃圾回收只清理索引和历史未引用的块。

正确性优先的预览实现仍会在显式扫描 / 同步时重新计算文件内容哈希，以免忽略修改时间未改变的文件；notify 去抖减少无事件扫描。单次传输目前串行，QUIC 本身复用连接。未来优化应先加入增量索引、传输并发、调速、取消和资源限制测试，再提供性能结论。
