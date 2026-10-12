import { useState, useEffect, useCallback, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  Cloud,
  House,
  Folder,
  Monitor,
  ArrowLeftRight,
  ArrowDownUp,
  Clock,
  Trash2,
  Settings,
  Upload,
  Plus,
  RefreshCw,
  ChevronRight,
  File,
  MoreHorizontal,
  X,
  ShieldCheck,
  HardDrive,
  ArrowUpRight,
  Check,
  Search,
  Download,
  History,
  Copy,
  Pencil,
} from "lucide-react";
type Item = {
  name: string;
  path: string;
  directory: boolean;
  size: number;
  modified: number;
  kind: string;
};
type Overview = {
  node_id: string;
  root: string;
  capacity: number;
  available: number;
  content_bytes: number;
  files: number;
  quota: number;
  index_error: string;
};
type Peer = { id: string; name: string; address: string };
type Job = {
  id: string;
  peer: string;
  status: string;
  bytes: number;
  error?: string;
  updated: number;
  bytes_per_second?: number;
};
type Trash = { id: string; path: string; deleted_at: number };
type Version = { id: number; created: number; entry: { size: number } };
type Form = {
  title: string;
  fields: { key: string; label: string; value: string }[];
  submit: (v: Record<string, string>) => Promise<void>;
};
const pages = [
  ["Home", "首页", House],
  ["My Files", "我的文件", Folder],
  ["Devices", "设备", Monitor],
  ["Sync", "同步", ArrowLeftRight],
  ["Transfers", "传输", ArrowDownUp],
  ["Recent", "最近使用", Clock],
  ["Trash", "回收站", Trash2],
  ["Settings", "设置", Settings],
] as const;
const req = <T,>(op: string, args: Record<string, unknown> = {}) =>
  invoke<T>("request", { op, args });
const bytes = (n: number) => {
  if (!n) return "0 B";
  const i = Math.min(4, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${["B", "KB", "MB", "GB", "TB"][i]}`;
};
const date = (n: number) => new Date(n * 1000).toLocaleString("zh-CN");

export default function App() {
  const [page, setPage] = useState("Home"),
    [path, setPath] = useState(""),
    [items, setItems] = useState<Item[]>([]),
    [overview, setOverview] = useState<Overview>(),
    [peers, setPeers] = useState<Peer[]>([]),
    [jobs, setJobs] = useState<Job[]>([]),
    [trash, setTrash] = useState<Trash[]>([]),
    [query, setQuery] = useState(""),
    [sort, setSort] = useState("name"),
    [message, setMessage] = useState(""),
    [busy, setBusy] = useState(false),
    [selected, setSelected] = useState<Item>(),
    [preview, setPreview] = useState<{ kind: string; content: string }>(),
    [versions, setVersions] = useState<Version[]>([]),
    [modal, setModal] = useState<Form>(),
    [theme, setTheme] = useState(localStorage.getItem("theme") || "system"),
    [listening, setListening] = useState(false);
  const refresh = useCallback(async () => {
    try {
      const [o, p, j] = await Promise.all([
        req<Overview>("overview"),
        req<Peer[]>("peers"),
        req<Job[]>("jobs"),
      ]);
      setOverview(o);
      if (o.index_error) setMessage(`本地索引需要处理：${o.index_error}`);
      setPeers(p);
      setJobs(j);
      if (page === "Trash") setTrash(await req("trash"));
      else
        setItems(
          await req<Item[]>(
            page === "Recent" ? "recent" : query ? "search" : "list",
            query ? { query } : { path },
          ),
        );
    } catch (e) {
      setMessage(String(e));
    }
  }, [page, path, query]);
  const [remoteItems, setRemoteItems] = useState<Item[]>(),
    [remotePeer, setRemotePeer] = useState(""),
    [remotePath, setRemotePath] = useState("");
  const [discovered, setDiscovered] = useState<
    { name: string; addresses: string[]; fingerprint: string }[]
  >([]);
  const browseRemote = async (peer: string, folder: string) => {
    const files = await req<Item[]>("remote_list", { peer, path: folder });
    setRemotePeer(peer);
    setRemotePath(folder);
    setRemoteItems(files);
  };
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, [refresh]);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("theme", theme);
  }, [theme]);
  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setMessage("");
    try {
      await fn();
      await refresh();
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };
  const navigate = (p: string) => {
    setPage(p);
    setSelected(undefined);
    setPreview(undefined);
    setVersions([]);
    setQuery("");
  };
  const importPaths = async (sources: string[]) => {
    for (const source of sources) {
      const name = source.split(/[\\/]/).pop()!;
      await req("import", { source, path: path ? `${path}/${name}` : name });
    }
  };
  useEffect(() => {
    let closed = false;
    let dispose: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const payload = e.payload;
        if (payload.type === "drop") void run(() => importPaths(payload.paths));
      })
      .then((u) => {
        if (closed) u();
        else dispose = u;
      })
      .catch(() => {});
    return () => {
      closed = true;
      dispose?.();
    };
  }, [path]);
  const importFiles = () =>
    void run(async () => {
      const files = await open({ multiple: true });
      if (files) await importPaths(Array.isArray(files) ? files : [files]);
    });
  const form = (
    title: string,
    fields: Form["fields"],
    submit: Form["submit"],
  ) => setModal({ title, fields, submit });
  const field = (key: string, label: string, value = "") => ({
    key,
    label,
    value,
  });
  const openItem = (item: Item) => {
    if (item.directory) {
      setPath(item.path);
      navigate("My Files");
    } else {
      setSelected(item);
      setVersions([]);
      void run(async () =>
        setPreview(await req("preview", { path: item.path })),
      );
    }
  };
  const sorted = [...items].sort(
    (a, b) =>
      Number(b.directory) - Number(a.directory) ||
      (sort === "size"
        ? b.size - a.size
        : sort === "modified"
          ? b.modified - a.modified
          : a.name.localeCompare(b.name)),
  );
  const title = pages.find((p) => p[0] === page)?.[1];
  return (
    <div className="shell">
      <aside>
        <div className="brand">
          <span>
            <Cloud size={25} />
          </span>
          <div>
            HoriDrive<small>PERSONAL CLOUD</small>
          </div>
        </div>
        <div className="workspace">
          <b>H</b>
          <div>
            我的私人空间<small>本机存储 · 由你掌控</small>
          </div>
          <ShieldCheck size={16} />
        </div>
        <div className="nav-label">WORKSPACE</div>
        <nav>
          {pages.map(([id, label, Icon]) => (
            <button
              key={id}
              className={page === id ? "active" : ""}
              onClick={() => navigate(id)}
            >
              <Icon size={19} />
              {label}
            </button>
          ))}
        </nav>
        <div className="aside-footer">
          <p>
            <i /> 本地文件可用
          </p>
          <p>Your Cloud. Your Rules.</p>
          <small>HoriDrive 0.1 · Preview</small>
        </div>
      </aside>
      <main>
        <header>
          <div>
            我的空间 <ChevronRight size={14} /> {title}
          </div>
          <div>
            <span className="privacy">
              <ShieldCheck size={14} /> 无遥测
            </span>
            <button
              title="重新索引"
              disabled={busy}
              onClick={() => void run(() => req("scan"))}
            >
              <RefreshCw size={17} />
            </button>
            <b className="avatar">H</b>
          </div>
        </header>
        <div className="content">
          {message && (
            <div role="alert" className="notice">
              <span>{message}</span>
              <button onClick={() => setMessage("")}>
                <X size={17} />
              </button>
            </div>
          )}
          <div className="heading">
            <div>
              <div className="eyebrow">YOUR PERSONAL CLOUD</div>
              <h1>{page === "Home" ? "每一份文件，都在你手中。" : title}</h1>
              <p>
                {page === "Home"
                  ? "自己的硬盘，自己的空间。安心保存，自由连接。"
                  : page === "Devices"
                    ? "双方交换公开证书，核对完整指纹，明确授权。"
                    : page === "Sync"
                      ? "双向同步保留冲突副本，离线文件始终可用。"
                      : "管理你自己的文件与设备。"}
              </p>
            </div>
            {["Home", "My Files", "Recent"].includes(page) && (
              <button
                className="primary"
                disabled={busy || !overview}
                onClick={importFiles}
              >
                <Upload size={16} /> 导入文件
              </button>
            )}
          </div>
          {page === "Home" && (
            <>
              <section className="hero">
                <div>
                  <span className="hero-tag">
                    <i /> LOCAL FIRST
                  </span>
                  <h2>
                    你的云盘。
                    <br />
                    从这里开始。
                  </h2>
                  <p>
                    文件保存在你的设备上。
                    <br />
                    连接信任的设备，让空间自由延伸。
                  </p>
                  <button onClick={() => navigate("My Files")}>
                    打开我的文件 <ArrowUpRight size={15} />
                  </button>
                </div>
                <div className="cloud-art">
                  <div className="orbit" />
                  <div className="orbit second" />
                  <div className="cloud-disc">
                    <Cloud size={84} strokeWidth={1.3} />
                    <span>
                      <Check size={20} />
                    </span>
                  </div>
                  <div className="tile file-tile">
                    <File size={25} />
                  </div>
                  <div className="tile shield-tile">
                    <ShieldCheck size={25} />
                  </div>
                </div>
                <small>OWN YOUR SPACE.</small>
              </section>
              <div className="stats">
                <Stat
                  icon={<HardDrive />}
                  label="当前磁盘容量"
                  value={overview ? bytes(overview.capacity) : "—"}
                  note={
                    overview
                      ? `可用 ${bytes(overview.available)}`
                      : "等待本地核心"
                  }
                />
                <Stat
                  icon={<Folder />}
                  label="我的文件"
                  value={overview ? String(overview.files) : "—"}
                  note={
                    overview
                      ? `历史与分块 ${bytes(overview.content_bytes)}`
                      : "读取真实索引"
                  }
                />
                <Stat
                  icon={<Monitor />}
                  label="已信任设备"
                  value={String(peers.length)}
                  note="在线状态在连接时验证"
                />
                <Stat
                  icon={<ArrowLeftRight />}
                  label="已完成同步"
                  value={String(
                    jobs.filter((j) => j.status === "complete").length,
                  )}
                  note={`${jobs.filter((j) => j.status === "failed").length} 个任务需重试`}
                />
              </div>
              <div className="section-title">
                <h2>我的文件</h2>
                <button onClick={() => navigate("My Files")}>
                  查看全部 <ChevronRight size={15} />
                </button>
              </div>
              {sorted.length ? (
                <div className="cards">
                  {sorted.slice(0, 4).map((item) => (
                    <button key={item.path} onClick={() => openItem(item)}>
                      {item.directory ? (
                        <Folder size={34} fill="currentColor" />
                      ) : (
                        <File size={34} />
                      )}
                      <strong>{item.name}</strong>
                      <span>
                        {item.directory ? "文件夹" : bytes(item.size)}
                      </span>
                    </button>
                  ))}
                </div>
              ) : (
                <Empty
                  icon={<Folder />}
                  title="这里是你的空间"
                  text="导入文件或拖拽文件到窗口，开始使用自己的私人云盘。"
                />
              )}
              <div className="local-note">
                <ShieldCheck size={21} />
                <div>
                  <strong>默认仅在本机运行</strong>
                  <p>
                    连接需双方授权。QUIC / TLS
                    保护传输；本地磁盘内容目前未加密。
                  </p>
                </div>
              </div>
            </>
          )}
          {["My Files", "Recent"].includes(page) && (
            <>
              <div className="toolbar">
                <div className="crumbs">
                  <button onClick={() => setPath("")}>
                    <House size={16} />
                  </button>
                  {path
                    .split("/")
                    .filter(Boolean)
                    .map((part, i) => (
                      <button
                        key={i}
                        onClick={() =>
                          setPath(
                            path
                              .split("/")
                              .slice(0, i + 1)
                              .join("/"),
                          )
                        }
                      >
                        <ChevronRight size={12} />
                        {part}
                      </button>
                    ))}
                </div>
                <div>
                  <label className="search">
                    <Search size={15} />
                    <input
                      placeholder="搜索文件"
                      value={query}
                      onChange={(e) => setQuery(e.target.value)}
                    />
                  </label>
                  <select
                    aria-label="排序"
                    value={sort}
                    onChange={(e) => setSort(e.target.value)}
                  >
                    <option value="name">名称</option>
                    <option value="modified">修改时间</option>
                    <option value="size">大小</option>
                  </select>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      form(
                        "新建文件夹",
                        [field("name", "文件夹名称")],
                        async (v) => {
                          await req("mkdir", {
                            path: path ? `${path}/${v.name}` : v.name,
                          });
                        },
                      )
                    }
                  >
                    <Plus size={16} />
                    新建
                  </button>
                </div>
              </div>
              <div className="table">
                <div className="table-head">
                  <span>名称</span>
                  <span>修改时间</span>
                  <span>大小</span>
                  <span />
                </div>
                {sorted.map((item) => (
                  <div className="file-row" key={item.path}>
                    <button onClick={() => openItem(item)}>
                      {item.directory ? (
                        <Folder
                          className="folder-icon"
                          fill="currentColor"
                          size={24}
                        />
                      ) : (
                        <File size={24} />
                      )}
                      <span>
                        {item.name}
                        <small>{query ? item.path : item.kind}</small>
                      </span>
                    </button>
                    <span>{date(item.modified)}</span>
                    <span>{item.directory ? "—" : bytes(item.size)}</span>
                    <button
                      title="文件操作"
                      onClick={() => {
                        setSelected(item);
                        setPreview(undefined);
                        setVersions([]);
                      }}
                    >
                      <MoreHorizontal size={20} />
                    </button>
                  </div>
                ))}
                {!items.length && (
                  <Empty
                    icon={<Folder />}
                    title="没有文件"
                    text="导入文件，或新建文件夹来整理你的空间。"
                  />
                )}
              </div>
              <div className="table-footer">
                {items.length} 个项目<span>拖拽文件到窗口即可导入</span>
              </div>
            </>
          )}
          {page === "Devices" && (
            <>
              <section className="panel">
                <div className="section-title">
                  <h2>局域网发现</h2>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        setDiscovered(await req("discover"));
                      })
                    }
                  >
                    扫描设备
                  </button>
                </div>
                <p className="muted">
                  发现结果只提供地址提示。仍需交换公开证书并独立核对指纹；接收端使用局域网地址监听时会发布
                  mDNS 公告。
                </p>
                {discovered.map((d) => (
                  <div className="record" key={d.fingerprint}>
                    <Monitor />
                    <div>
                      <strong>{d.name}</strong>
                      <p>{d.addresses.join(" · ")}</p>
                      <small className="mono">{d.fingerprint}</small>
                    </div>
                  </div>
                ))}
              </section>
              <section className="panel">
                <div className="section-title">
                  <h2>当前设备</h2>
                  <span className="pill">本机</span>
                </div>
                <p className="mono">{overview?.node_id}</p>
                <p className="muted">{overview?.root}</p>
                <div className="actions">
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        const dest = await save({
                          defaultPath: "HoriDrive-device.json",
                        });
                        if (dest) await req("identity_export", { dest });
                      })
                    }
                  >
                    <Download size={16} /> 导出公开证书
                  </button>
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        const source = await open({ multiple: false });
                        if (typeof source !== "string") return;
                        const info = await req<{ fingerprint: string }>(
                          "identity_inspect",
                          { source },
                        );
                        form(
                          "核对指纹并添加设备",
                          [
                            field(
                              "fingerprint",
                              `通过其他可靠渠道核对并输入完整指纹：${info.fingerprint}`,
                            ),
                            field(
                              "address",
                              "地址（IP:端口）",
                              "127.0.0.1:44330",
                            ),
                            field("name", "设备名称", "我的设备"),
                          ],
                          async (v) => {
                            await req("trust", { source, ...v });
                            setListening(false);
                          },
                        );
                      })
                    }
                  >
                    <Plus size={16} /> 添加设备
                  </button>
                </div>
              </section>
              <section className="panel">
                <h2>接收连接</h2>
                <p className="muted">
                  默认关闭。只接受已授权证书，双方都需添加对方。授权变更后重新启动接收。
                </p>
                <button
                  className="secondary"
                  disabled={busy}
                  onClick={() => {
                    if (listening)
                      void run(async () => {
                        await req("stop");
                        setListening(false);
                      });
                    else
                      form(
                        "启动接收服务",
                        [
                          field(
                            "address",
                            "监听 IP:端口（开放局域网请使用局域网 IP）",
                            "127.0.0.1:44330",
                          ),
                        ],
                        async (v) => {
                          await req("listen", v);
                          setListening(true);
                        },
                      );
                  }}
                >
                  {listening ? "停止接收" : "启动接收"}
                </button>
              </section>
              {peers.map((p) => (
                <div className="record" key={p.id}>
                  <Monitor />
                  <div>
                    <strong>{p.name}</strong>
                    <p>{p.address} · 待连接验证</p>
                    <small className="mono">{p.id}</small>
                  </div>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        await req("probe", { peer: p.id });
                        setMessage(`${p.name} 已通过加密连接验证。`);
                      })
                    }
                  >
                    检查连接
                  </button>
                  <button
                    title="撤销授权"
                    onClick={() =>
                      form("撤销设备授权", [], async () => {
                        await req("revoke", { peer: p.id });
                        setListening(false);
                      })
                    }
                  >
                    <Trash2 size={18} />
                  </button>
                </div>
              ))}
              {!peers.length && (
                <Empty
                  icon={<Monitor />}
                  title="连接你的另一台设备"
                  text="双方分别导出设备证书、核对完整指纹并添加信任，然后启动接收服务。"
                />
              )}
            </>
          )}
          {page === "Sync" && (
            <>
              <section className="panel">
                <h2>同步文件夹</h2>
                <p className="mono">{overview?.root}</p>
                <p className="muted">
                  双向同步此空间的全部文件。也可浏览远程目录，仅下载需要的文件。远端离线时只能访问已下载的本地内容。桌面后台自动重连尚未实现。
                </p>
              </section>
              {peers.map((p) => (
                <div className="record" key={p.id}>
                  <ArrowLeftRight />
                  <div>
                    <strong>{p.name}</strong>
                    <p>{p.address}</p>
                  </div>
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => void run(() => req("sync", { peer: p.id }))}
                  >
                    <RefreshCw size={16} /> 双向同步
                  </button>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() => void run(() => browseRemote(p.id, ""))}
                  >
                    浏览远程文件
                  </button>
                </div>
              ))}
              {remoteItems && (
                <section className="panel">
                  <div className="section-title">
                    <h2>远程目录 · {remotePath || "根目录"}</h2>
                    <button
                      className="secondary"
                      onClick={() =>
                        void run(() =>
                          browseRemote(
                            remotePeer,
                            remotePath.split("/").slice(0, -1).join("/"),
                          ),
                        )
                      }
                    >
                      上一级
                    </button>
                  </div>
                  <p className="muted">
                    这是上次连接获取的目录。每次浏览或下载都会重新验证远端连接。
                  </p>
                  {remoteItems.map((item) => (
                    <div className="record" key={item.path}>
                      {item.directory ? <Folder /> : <File />}
                      <div>
                        <strong>{item.name}</strong>
                        <p>{item.directory ? "文件夹" : bytes(item.size)}</p>
                      </div>
                      <button
                        className="secondary"
                        disabled={busy}
                        onClick={() =>
                          void run(async () => {
                            if (item.directory)
                              await browseRemote(remotePeer, item.path);
                            else {
                              await req("remote_fetch", {
                                peer: remotePeer,
                                path: item.path,
                              });
                              setMessage("已下载到本地空间，可离线访问。");
                            }
                          })
                        }
                      >
                        {item.directory ? "打开" : "下载到本地"}
                      </button>
                    </div>
                  ))}
                  {!remoteItems.length && (
                    <p className="muted">远程文件夹为空。</p>
                  )}
                </section>
              )}
              {!peers.length && (
                <Empty
                  icon={<ArrowLeftRight />}
                  title="先连接一台设备"
                  text="在设备页面完成双方授权，然后开始同步。"
                />
              )}
            </>
          )}
          {page === "Transfers" &&
            (jobs.length ? (
              jobs.map((j) => (
                <div className="record" key={j.id}>
                  <ArrowDownUp />
                  <div>
                    <strong>
                      {j.status === "complete"
                        ? "同步完成"
                        : j.status === "failed"
                          ? "同步失败"
                          : j.status === "cancelled"
                            ? "已取消"
                            : "传输中"}
                    </strong>
                    <p>
                      {j.peer} · {bytes(j.bytes)} ·{" "}
                      {bytes(j.bytes_per_second || 0)}/s · {date(j.updated)}
                    </p>
                    {j.error && <p>{j.error}</p>}
                  </div>
                  <span className="pill">{j.status}</span>
                  {j.status === "running" && (
                    <button
                      className="secondary"
                      onClick={() =>
                        void req("cancel", { id: j.id }).catch((e) =>
                          setMessage(String(e)),
                        )
                      }
                    >
                      取消
                    </button>
                  )}
                </div>
              ))
            ) : (
              <Empty
                icon={<ArrowDownUp />}
                title="还没有传输任务"
                text="开始同步后，真实任务记录会显示在这里。"
              />
            ))}
          {page === "Trash" &&
            (trash.length ? (
              trash.map((t) => (
                <div className="record" key={t.id}>
                  <Trash2 />
                  <div>
                    <strong>{t.path}</strong>
                    <p>{date(t.deleted_at)}</p>
                  </div>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      void run(() => req("restore_trash", { id: t.id }))
                    }
                  >
                    恢复
                  </button>
                </div>
              ))
            ) : (
              <Empty
                icon={<Trash2 />}
                title="回收站是空的"
                text="删除的文件会先保存在这里。恢复不会覆盖现有文件。"
              />
            ))}
          {page === "Settings" && (
            <>
              <section className="panel">
                <h2>外观</h2>
                <p className="muted">选择适合你的界面。</p>
                <div className="actions">
                  {[
                    ["system", "跟随系统"],
                    ["light", "浅色"],
                    ["dark", "深色"],
                  ].map(([id, label]) => (
                    <button
                      key={id}
                      className={`secondary ${theme === id ? "chosen" : ""}`}
                      onClick={() => setTheme(id)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </section>
              <section className="panel">
                <h2>存储与版本</h2>
                <p className="muted">
                  每个路径最多保留 20
                  个历史版本；回收站不自动清空。配额约束分块存储，0 表示不限。
                </p>
                <div className="actions">
                  <button
                    className="secondary"
                    onClick={() =>
                      form(
                        "设置内容存储配额",
                        [
                          field(
                            "bytes",
                            "字节；0 表示不限",
                            String(overview?.quota || 0),
                          ),
                        ],
                        async (v) => {
                          await req("quota", { bytes: Number(v.bytes) });
                        },
                      )
                    }
                  >
                    配置配额
                  </button>
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        const n = await req<number>("prune");
                        setMessage(`已清理 ${bytes(n)} 未引用分块。`);
                      })
                    }
                  >
                    清理未引用分块
                  </button>
                </div>
              </section>
              <section className="panel">
                <h2>关于 HoriDrive</h2>
                <p>Your Cloud. Your Rules.</p>
                <p className="muted">
                  0.1.0 Preview · MIT License
                  <br />
                  无默认遥测。QUIC / TLS 1.3
                  保护设备间传输；本地磁盘内容未加密。
                  <br />
                  支持手动 IP 和已配置的 Tailscale 网络；移动端和原生 NAT
                  穿透尚未实现。
                </p>
                <p className="mono">{overview?.root}</p>
              </section>
            </>
          )}
          {page === "Settings" && (
            <section className="panel">
              <h2>传输限速</h2>
              <p className="muted">
                限速约束本机发起的同步，按分块调节；0
                表示不限。接收服务不提供全局带宽限制。
              </p>
              <button
                className="secondary"
                onClick={() =>
                  form(
                    "配置同步传输限速",
                    [field("bytes", "每秒字节数；0 表示不限", "0")],
                    async (v) => {
                      await req("rate_limit", { bytes: Number(v.bytes) });
                    },
                  )
                }
              >
                设置限速
              </button>
            </section>
          )}
        </div>
        <footer>
          <span>
            <i />{" "}
            {busy ? "正在处理…" : overview ? "本地空间就绪" : "等待本地核心"}
          </span>
          <span>
            {bytes(
              jobs
                .filter((j) => j.status === "running")
                .reduce((sum, j) => sum + (j.bytes_per_second || 0), 0),
            )}
            /s · {overview ? `${bytes(overview.available)} 可用` : "—"} ·{" "}
            {listening ? "接收服务已启动" : "接收服务关闭"}
          </span>
        </footer>
      </main>
      {selected && (
        <section className="drawer">
          <div className="section-title">
            <h2>{selected.name}</h2>
            <button onClick={() => setSelected(undefined)}>
              <X />
            </button>
          </div>
          <div className="actions file-actions">
            <button
              className="secondary"
              onClick={() =>
                void run(async () => {
                  const dest = selected.directory
                    ? await open({ directory: true })
                    : await save({ defaultPath: selected.name });
                  if (typeof dest === "string")
                    await req("export", {
                      path: selected.path,
                      dest: selected.directory
                        ? `${dest}/${selected.name}`
                        : dest,
                    });
                })
              }
            >
              <Download size={15} />
              导出
            </button>
            {[
              ["copy", "复制", Copy],
              ["move", "重命名 / 移动", Pencil],
            ].map(([op, label, Icon]) => (
              <button
                className="secondary"
                key={String(op)}
                onClick={() =>
                  form(
                    String(label),
                    [field("to", "目标相对路径", selected.path)],
                    async (v) => {
                      await req(String(op), { from: selected.path, to: v.to });
                    },
                  )
                }
              >
                <Icon size={15} />
                {String(label)}
              </button>
            ))}
            <button
              className="secondary"
              onClick={() =>
                void run(async () => {
                  setVersions(await req("versions", { path: selected.path }));
                  setPreview(undefined);
                })
              }
            >
              <History size={15} />
              版本
            </button>
            <button
              className="secondary danger"
              onClick={() =>
                form("移入回收站", [], async () => {
                  await req("delete", { path: selected.path });
                  setSelected(undefined);
                })
              }
            >
              <Trash2 size={15} />
              删除
            </button>
          </div>
          {preview?.kind === "image" ? (
            <img className="preview-image" src={preview.content} />
          ) : (
            preview && <pre>{preview.content}</pre>
          )}
          {versions.map((v) => (
            <div className="version" key={v.id}>
              <span>
                {date(v.created)}
                <small>{bytes(v.entry.size)}</small>
              </span>
              <button
                className="secondary"
                onClick={() =>
                  void run(() =>
                    req("restore_version", { path: selected.path, id: v.id }),
                  )
                }
              >
                恢复
              </button>
            </div>
          ))}
        </section>
      )}
      {modal && (
        <div className="backdrop">
          <form
            className="modal"
            onSubmit={(e) => {
              e.preventDefault();
              const data = Object.fromEntries(
                new FormData(e.currentTarget).entries(),
              ) as Record<string, string>;
              void run(async () => {
                await modal.submit(data);
                setModal(undefined);
              });
            }}
          >
            <div className="section-title">
              <h2>{modal.title}</h2>
              <button type="button" onClick={() => setModal(undefined)}>
                <X size={20} />
              </button>
            </div>
            {modal.fields.map((f) => (
              <label key={f.key}>
                {f.label}
                <input autoFocus required name={f.key} defaultValue={f.value} />
              </label>
            ))}
            {!modal.fields.length && <p>确认执行此操作。</p>}
            <div className="modal-actions">
              <button
                className="secondary"
                type="button"
                onClick={() => setModal(undefined)}
              >
                取消
              </button>
              <button className="primary" disabled={busy} type="submit">
                {busy ? "处理中…" : "确认"}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
function Stat({
  icon,
  label,
  value,
  note,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  note: string;
}) {
  return (
    <div className="stat">
      <div>
        {icon}
        {label}
      </div>
      <strong>{value}</strong>
      <p>{note}</p>
    </div>
  );
}
function Empty({
  icon,
  title,
  text,
}: {
  icon: ReactNode;
  title: string;
  text: string;
}) {
  return (
    <div className="empty">
      <span>{icon}</span>
      <strong>{title}</strong>
      <p>{text}</p>
    </div>
  );
}
