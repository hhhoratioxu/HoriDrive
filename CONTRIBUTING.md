# 参与贡献

使用 Rust Stable 和 Node.js 22+。提交前运行 `cargo fmt --all --check`、`cargo clippy -- -D warnings`、`cargo test`，并在 desktop 中运行 `npm ci && npm run build`。

新增同步逻辑必须附加真实临时目录测试；网络协议需覆盖未经授权的节点、路径攻击和完整性失败。涉及文件替换 / 删除的修改必须解释崩溃恢复行为。请勿使用真实个人文件做测试，不提交私钥、设备数据目录或凭据。不要修改独立的 HoriLink 项目。

功能 PR 描述触发条件、最终行为、运行的检查和未验证的平台。性能声明需要公开硬件、网络、文件分布、参数和测试过程。安全问题按 SECURITY.md 处理。
