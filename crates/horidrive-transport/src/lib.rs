//! QUIC / TLS 1.3 with mutual authentication and explicit certificate exchange.
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use horidrive_core::Core;
use horidrive_storage::{now, Entry, BLOCK_SIZE};
use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    Connection, Endpoint,
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    RootCertStore,
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub type SharedCore = Arc<Mutex<Core>>;
pub struct Receiver {
    endpoint: Endpoint,
    advertisement: Option<discovery::Advertisement>,
}
impl Receiver {
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }
    pub fn close(&self, code: quinn::VarInt, reason: &[u8]) {
        self.endpoint.close(code, reason);
    }
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"Receiver dropped");
        self.advertisement.take();
    }
}
pub mod discovery;
const ALPN: &[u8] = b"horidrive/1";
const FRAME_LIMIT: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Serialize, Deserialize)]
pub struct PublicIdentity {
    pub id: String,
    pub certificate: String,
}
impl PublicIdentity {
    pub fn cert(&self) -> Result<CertificateDer<'static>> {
        if self.certificate.len() > 16384 {
            bail!("Certificate too large");
        }
        Ok(CertificateDer::from(STANDARD.decode(&self.certificate)?))
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(blake3::hash(self.cert()?.as_ref()).to_hex().to_string())
    }
}
#[derive(Clone)]
pub struct Identity {
    pub public: PublicIdentity,
    key: Vec<u8>,
}
impl Identity {
    pub fn generate(id: &str) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["horidrive.local".into()])?;
        Ok(Self {
            public: PublicIdentity {
                id: id.into(),
                certificate: STANDARD.encode(cert.cert.der()),
            },
            key: cert.signing_key.serialize_der(),
        })
    }
    pub fn load(core: &Core) -> Result<Self> {
        let entry = keyring::Entry::new("HoriDrive", &core.node_id)?;
        if let Some(public) = core.store.setting("identity")? {
            let public: PublicIdentity = serde_json::from_str(&public)?;
            let key=STANDARD.decode(entry.get_password().context("Device private key unavailable in OS credential store; do not silently replace identity")?)?;
            Ok(Self { public, key })
        } else {
            let identity = Self::generate(&core.node_id)?;
            entry
                .set_password(&STANDARD.encode(&identity.key))
                .context("Cannot secure private key in OS credential store")?;
            core.store
                .set_setting("identity", &serde_json::to_string(&identity.public)?)?;
            Ok(identity)
        }
    }
    fn key(&self) -> PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.key.clone()).into()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub address: String,
    pub certificate: String,
}
pub fn peers(core: &Core) -> Result<Vec<Peer>> {
    Ok(core
        .store
        .setting("peers")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}
pub fn trust(
    core: &Core,
    public: &PublicIdentity,
    fingerprint: &str,
    name: &str,
    address: &str,
) -> Result<Peer> {
    let id = public.fingerprint()?;
    if fingerprint != id {
        bail!("Fingerprint confirmation does not match certificate");
    }
    if name.len() > 128 || address.len() > 256 || address.is_empty() {
        bail!("Invalid device name or address");
    }
    let mut list = peers(core)?;
    list.retain(|p| p.id != id);
    let peer = Peer {
        id,
        name: name.into(),
        address: address.into(),
        certificate: public.certificate.clone(),
    };
    list.push(peer.clone());
    core.store
        .set_setting("peers", &serde_json::to_string(&list)?)?;
    Ok(peer)
}
pub fn revoke(core: &Core, id: &str) -> Result<()> {
    let mut list = peers(core)?;
    list.retain(|p| p.id != id);
    core.store
        .set_setting("peers", &serde_json::to_string(&list)?)
}
pub fn read_public(path: &Path) -> Result<PublicIdentity> {
    let data = std::fs::read(path)?;
    if data.len() > 32768 {
        bail!("Identity file too large");
    }
    let p: PublicIdentity = serde_json::from_slice(&data)?;
    p.cert()?;
    Ok(p)
}
fn roots(list: &[Peer]) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for p in list {
        roots.add(
            PublicIdentity {
                id: p.id.clone(),
                certificate: p.certificate.clone(),
            }
            .cert()?,
        )?;
    }
    Ok(roots)
}
fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.max_concurrent_bidi_streams(8u32.into());
    t.max_concurrent_uni_streams(0u32.into());
    t.keep_alive_interval(Some(Duration::from_secs(5)));
    t.max_idle_timeout(Some(Duration::from_secs(20).try_into().unwrap()));
    Arc::new(t)
}
pub fn serve(core: SharedCore, identity: &Identity, address: SocketAddr) -> Result<Receiver> {
    let list = peers(&core.lock().unwrap())?;
    if list.is_empty() {
        bail!("Trust at least one device before starting reception");
    }
    let verifier =
        rustls::server::WebPkiClientVerifier::builder(Arc::new(roots(&list)?)).build()?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_client_cert_verifier(verifier)
    .with_single_cert(vec![identity.public.cert()?], identity.key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    config.transport_config(transport());
    let endpoint = Endpoint::server(config, address)?;
    let listener = endpoint.clone();
    tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(16));
        while let Some(incoming) = listener.accept().await {
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                incoming.refuse();
                continue;
            };
            let core = core.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = handle_incoming(incoming, core).await {
                    tracing::debug!(error=%e,"Peer connection ended");
                }
            });
        }
    });
    let local = endpoint.local_addr()?;
    let advertisement = if local.ip().is_loopback() {
        None
    } else {
        match discovery::Advertisement::start(local, &identity.public.fingerprint()?) {
            Ok(ad) => Some(ad),
            Err(_) => {
                tracing::warn!("mDNS unavailable; manual address connection remains available");
                None
            }
        }
    };
    Ok(Receiver {
        endpoint,
        advertisement,
    })
}
fn fingerprint(connection: &Connection) -> Result<String> {
    let identity = connection
        .peer_identity()
        .context("Missing TLS peer identity")?;
    let certs = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow::anyhow!("Unsupported TLS identity"))?;
    Ok(
        blake3::hash(certs.first().context("Missing certificate")?.as_ref())
            .to_hex()
            .to_string(),
    )
}
#[derive(Serialize, Deserialize)]
enum Request {
    Ping,
    Manifest,
    Has { hash: String },
    List { path: String },
    Get { hash: String },
    Put { hash: String, size: usize },
    Apply { entry: Entry },
}
#[derive(Serialize, Deserialize)]
struct Response {
    ok: bool,
    error: Option<String>,
    data: serde_json::Value,
    size: usize,
}
async fn write_json<T: Serialize>(stream: &mut quinn::SendStream, value: &T) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    if data.len() > FRAME_LIMIT {
        bail!("Protocol frame limit exceeded");
    }
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(&data).await?;
    Ok(())
}
async fn read_json<T: serde::de::DeserializeOwned>(stream: &mut quinn::RecvStream) -> Result<T> {
    let len = stream.read_u32().await? as usize;
    if len > FRAME_LIMIT {
        bail!("Protocol frame limit exceeded");
    }
    let mut data = vec![0; len];
    stream.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}
async fn handle_incoming(incoming: quinn::Incoming, core: SharedCore) -> Result<()> {
    let connection = tokio::time::timeout(TIMEOUT, incoming).await??;
    let id = fingerprint(&connection)?;
    loop {
        let (send, recv) = match connection.accept_bi().await {
            Ok(stream) => stream,
            Err(_) => break,
        };
        // Check exact certificate pin on every request, so revocation affects existing connections.
        if !peers(&core.lock().unwrap())?.iter().any(|p| p.id == id) {
            connection.close(1u32.into(), b"Authorization revoked");
            break;
        }
        tokio::time::timeout(TIMEOUT, respond(send, recv, core.clone())).await??;
    }
    Ok(())
}
async fn respond(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    core: SharedCore,
) -> Result<()> {
    let req: Request = read_json(&mut recv).await?;
    let mut blob = vec![];
    let incoming = if let Request::Put { size, .. } = &req {
        if *size > BLOCK_SIZE {
            bail!("Block too large");
        }
        let mut data = vec![0; *size];
        recv.read_exact(&mut data).await?;
        data
    } else {
        vec![]
    };
    // Keep filesystem/SQLite work off the Tokio I/O threads.
    let result = tokio::task::spawn_blocking(move || -> Result<(serde_json::Value, Vec<u8>)> {
        let mut c = core.lock().unwrap();
        let mut blob = vec![];
        let data = match req {
            Request::Ping => serde_json::json!({"node_id":c.node_id}),
            Request::Manifest => {
                c.scan()?;
                serde_json::to_value(c.store.entries()?)?
            }
            Request::List { path } => serde_json::to_value(c.list(&path)?)?,
            Request::Get { hash } => {
                blob = c.store.block(&hash)?;
                serde_json::Value::Null
            }
            Request::Has { hash } => serde_json::json!(c.store.has_block(&hash)?),
            Request::Put { hash, .. } => {
                c.store.put_block(&hash, &incoming)?;
                serde_json::Value::Null
            }
            Request::Apply { entry } => {
                serde_json::json!(c.apply(&entry)?)
            }
        };
        Ok((data, blob))
    })
    .await?;
    let response = match result {
        Ok((data, bytes)) => {
            blob = bytes;
            Response {
                ok: true,
                error: None,
                data,
                size: blob.len(),
            }
        }
        Err(e) => Response {
            ok: false,
            error: Some(e.to_string()),
            data: serde_json::Value::Null,
            size: 0,
        },
    };
    write_json(&mut send, &response).await?;
    if !blob.is_empty() {
        send.write_all(&blob).await?;
    }
    send.finish()?;
    Ok(())
}

pub struct Client {
    endpoint: Endpoint,
    connection: Connection,
}
impl Client {
    pub async fn connect(identity: &Identity, peer: &Peer) -> Result<Self> {
        let addr = tokio::time::timeout(TIMEOUT, tokio::net::lookup_host(&peer.address))
            .await?
            .context("Cannot resolve device address")?
            .next()
            .context("No address found")?;
        let local: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()?;
        let mut endpoint = Endpoint::client(local)?;
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots(std::slice::from_ref(peer))?)
        .with_client_auth_cert(vec![identity.public.cert()?], identity.key())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
        config.transport_config(transport());
        endpoint.set_default_client_config(config);
        let connection = tokio::time::timeout(TIMEOUT, endpoint.connect(addr, "horidrive.local")?)
            .await
            .context(
                "Connection timed out; check address, UDP firewall, receiver and mutual trust",
            )??;
        if fingerprint(&connection)? != peer.id {
            connection.close(1u32.into(), b"Certificate mismatch");
            bail!("Device certificate pin mismatch");
        }
        Ok(Self {
            endpoint,
            connection,
        })
    }
    async fn rpc(&self, req: Request, blob: &[u8]) -> Result<(serde_json::Value, Vec<u8>)> {
        tokio::time::timeout(TIMEOUT, async {
            let (mut send, mut recv) = self.connection.open_bi().await?;
            write_json(&mut send, &req).await?;
            if !blob.is_empty() {
                send.write_all(blob).await?;
            }
            send.finish()?;
            let response: Response = read_json(&mut recv).await?;
            if !response.ok {
                bail!(
                    "Peer rejected request: {}",
                    response.error.unwrap_or_default()
                );
            }
            if response.size > BLOCK_SIZE {
                bail!("Oversized response block");
            }
            let mut data = vec![0; response.size];
            recv.read_exact(&mut data).await?;
            Ok((response.data, data))
        })
        .await
        .context("Transfer timed out; verified blocks are retained for retry")?
    }
    pub async fn ping(&self) -> Result<serde_json::Value> {
        Ok(self.rpc(Request::Ping, &[]).await?.0)
    }
    pub async fn manifest(&self) -> Result<Vec<Entry>> {
        Ok(serde_json::from_value(
            self.rpc(Request::Manifest, &[]).await?.0,
        )?)
    }
    pub async fn list(&self, path: &str) -> Result<serde_json::Value> {
        Ok(self.rpc(Request::List { path: path.into() }, &[]).await?.0)
    }
    pub async fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let data = self.rpc(Request::Get { hash: hash.into() }, &[]).await?.1;
        if blake3::hash(&data).to_hex().as_str() != hash {
            bail!("Received block hash mismatch");
        }
        Ok(data)
    }
    pub async fn put(&self, hash: &str, data: &[u8]) -> Result<()> {
        self.rpc(
            Request::Put {
                hash: hash.into(),
                size: data.len(),
            },
            data,
        )
        .await?;
        Ok(())
    }
    pub async fn has(&self, hash: &str) -> Result<bool> {
        Ok(serde_json::from_value(
            self.rpc(Request::Has { hash: hash.into() }, &[]).await?.0,
        )?)
    }
    pub async fn apply(&self, entry: &Entry) -> Result<()> {
        self.rpc(
            Request::Apply {
                entry: entry.clone(),
            },
            &[],
        )
        .await?;
        Ok(())
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"Done");
        self.endpoint.close(0u32.into(), b"Done");
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub peer: String,
    pub status: String,
    pub bytes: u64,
    pub error: Option<String>,
    pub updated: u64,
    #[serde(default)]
    pub bytes_per_second: u64,
}
fn save_job(core: &SharedCore, job: &Job) -> Result<()> {
    core.lock()
        .unwrap()
        .store
        .job(&job.id, &serde_json::to_string(job)?)
}
pub async fn fetch(
    core: SharedCore,
    identity: &Identity,
    peer: &Peer,
    path: &str,
) -> Result<Entry> {
    horidrive_core::validate(path, false)?;
    let client = Client::connect(identity, peer).await?;
    let e = client
        .manifest()
        .await?
        .into_iter()
        .find(|e| e.path == path && !e.deleted && !e.directory)
        .context("Remote file not found")?;
    for hash in &e.blocks {
        if !core.lock().unwrap().store.has_block(hash)? {
            let data = client.get(hash).await?;
            core.lock().unwrap().store.put_block(hash, &data)?;
        }
    }
    let mut c = core.lock().unwrap();
    c.scan()?;
    c.apply(&e)?;
    Ok(e)
}
pub async fn sync(core: SharedCore, identity: &Identity, peer: &Peer) -> Result<Job> {
    let mut job = Job {
        id: format!("sync-{}", peer.id),
        peer: peer.name.clone(),
        status: "running".into(),
        bytes: 0,
        error: None,
        updated: now(),
        bytes_per_second: 0,
    };
    core.lock()
        .unwrap()
        .store
        .set_setting(&format!("cancel:{}", job.id), "false")?;
    save_job(&core, &job)?;
    let result = sync_inner(core.clone(), identity, peer, &mut job).await;
    job.updated = now();
    job.status = if result.is_ok() { "complete" } else { "failed" }.into();
    job.error = result.as_ref().err().map(|e| e.to_string());
    if job.error.as_deref() == Some("Transfer cancelled") {
        job.status = "cancelled".into();
    }
    job.bytes_per_second = 0;
    save_job(&core, &job)?;
    result?;
    Ok(job)
}
pub fn cancel(core: &Core, id: &str) -> Result<()> {
    core.store.set_setting(&format!("cancel:{id}"), "true")
}
fn check_cancel(core: &SharedCore, job: &Job) -> Result<()> {
    if core
        .lock()
        .unwrap()
        .store
        .setting(&format!("cancel:{}", job.id))?
        .as_deref()
        == Some("true")
    {
        bail!("Transfer cancelled");
    }
    Ok(())
}
async fn progress(core: &SharedCore, job: &mut Job, started: std::time::Instant) -> Result<()> {
    let rate = core
        .lock()
        .unwrap()
        .store
        .setting("rate_limit_bps")?
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if rate > 0 {
        let expected = Duration::from_secs_f64(job.bytes as f64 / rate as f64);
        while started.elapsed() < expected {
            check_cancel(core, job)?;
            tokio::time::sleep(
                expected
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(100)),
            )
            .await;
        }
    }
    check_cancel(core, job)?;
    job.bytes_per_second = (job.bytes as f64 / started.elapsed().as_secs_f64().max(0.001)) as u64;
    job.updated = now();
    save_job(core, job)
}
fn order(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        a.deleted
            .cmp(&b.deleted)
            .then_with(|| {
                if a.deleted {
                    b.path
                        .matches('/')
                        .count()
                        .cmp(&a.path.matches('/').count())
                } else {
                    a.path
                        .matches('/')
                        .count()
                        .cmp(&b.path.matches('/').count())
                }
            })
            .then(b.directory.cmp(&a.directory))
            .then(a.path.cmp(&b.path))
    });
}
async fn sync_inner(
    core: SharedCore,
    identity: &Identity,
    peer: &Peer,
    job: &mut Job,
) -> Result<()> {
    let started = std::time::Instant::now();
    let client = Client::connect(identity, peer).await?;
    core.lock().unwrap().scan()?;
    let mut remote = client.manifest().await?;
    order(&mut remote);
    for e in &remote {
        check_cancel(&core, job)?;
        horidrive_core::validate(&e.path, false)?;
        let wanted = {
            let c = core.lock().unwrap();
            c.store
                .entry(&e.path)?
                .is_none_or(|l| !horidrive_core::dominates(&l.clock, &e.clock))
        };
        if !wanted {
            continue;
        }
        if !e.deleted && !e.directory {
            for h in &e.blocks {
                if !core.lock().unwrap().store.has_block(h)? {
                    let data = client.get(h).await?;
                    core.lock().unwrap().store.put_block(h, &data)?;
                    job.bytes += data.len() as u64;
                    progress(&core, job, started).await?;
                }
            }
        }
        core.lock().unwrap().apply(e)?;
    }
    let mut local = core.lock().unwrap().scan()?;
    order(&mut local);
    let remote = client.manifest().await?;
    let known: std::collections::HashSet<_> = remote
        .iter()
        .filter(|e| !e.deleted)
        .flat_map(|e| e.blocks.iter().cloned())
        .collect();
    for e in local {
        check_cancel(&core, job)?;
        if remote
            .iter()
            .any(|r| r.path == e.path && horidrive_core::dominates(&r.clock, &e.clock))
        {
            continue;
        }
        if !e.deleted && !e.directory {
            for h in &e.blocks {
                if !known.contains(h) && !client.has(h).await? {
                    let data = core.lock().unwrap().store.block(h)?;
                    client.put(h, &data).await?;
                    job.bytes += data.len() as u64;
                    progress(&core, job, started).await?;
                }
            }
        }
        client.apply(&e).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_preserves_verified_blocks_for_retry() -> Result<()> {
        let t = tempfile::tempdir()?;
        let a = Arc::new(Mutex::new(Core::open(&t.path().join("a"))?));
        let b = Arc::new(Mutex::new(Core::open(&t.path().join("b"))?));
        let ai = Identity::generate("a")?;
        let bi = Identity::generate("b")?;
        trust(
            &b.lock().unwrap(),
            &ai.public,
            &ai.public.fingerprint()?,
            "a",
            "127.0.0.1:1",
        )?;
        let receiver = serve(b.clone(), &bi, "127.0.0.1:0".parse()?)?;
        let peer = trust(
            &a.lock().unwrap(),
            &bi.public,
            &bi.public.fingerprint()?,
            "b",
            &receiver.local_addr()?.to_string(),
        )?;
        std::fs::write(
            a.lock().unwrap().root.join("cancel.bin"),
            vec![61; BLOCK_SIZE],
        )?;
        a.lock().unwrap().store.set_setting("rate_limit_bps", "1")?;
        let node = a.clone();
        let identity = ai.clone();
        let remote = peer.clone();
        let task = tokio::spawn(async move { sync(node, &identity, &remote).await });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if b.lock().unwrap().store.used()? > 0 {
                break;
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "No block received before cancellation"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel(&a.lock().unwrap(), &format!("sync-{}", peer.id))?;
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await??
            .is_err());
        assert!(!b.lock().unwrap().root.join("cancel.bin").exists());
        a.lock().unwrap().store.set_setting("rate_limit_bps", "0")?;
        let result = sync(a, &ai, &peer).await?;
        assert_eq!(result.bytes, 0);
        assert_eq!(
            std::fs::metadata(b.lock().unwrap().root.join("cancel.bin"))?.len(),
            BLOCK_SIZE as u64
        );
        drop(receiver);
        Ok(())
    }
    #[tokio::test]
    async fn mutual_tls_real_nodes_and_revocation() -> Result<()> {
        let t = tempfile::tempdir()?;
        let a = Arc::new(Mutex::new(Core::open(&t.path().join("a"))?));
        let b = Arc::new(Mutex::new(Core::open(&t.path().join("b"))?));
        let ai = Identity::generate("a")?;
        let bi = Identity::generate("b")?;
        trust(
            &b.lock().unwrap(),
            &ai.public,
            &ai.public.fingerprint()?,
            "a",
            "127.0.0.1:1",
        )?;
        let server = serve(b.clone(), &bi, "127.0.0.1:0".parse()?)?;
        let peer = trust(
            &a.lock().unwrap(),
            &bi.public,
            &bi.public.fingerprint()?,
            "b",
            &server.local_addr()?.to_string(),
        )?;
        std::fs::write(a.lock().unwrap().root.join("中文 #+.txt"), "hello peer")?;
        sync(a.clone(), &ai, &peer).await?;
        assert_eq!(
            std::fs::read_to_string(b.lock().unwrap().root.join("中文 #+.txt"))?,
            "hello peer"
        );
        a.lock().unwrap().delete("中文 #+.txt")?;
        sync(a.clone(), &ai, &peer).await?;
        assert!(!b.lock().unwrap().root.join("中文 #+.txt").exists());
        let client = Client::connect(&ai, &peer).await?;
        client.ping().await?;
        revoke(&b.lock().unwrap(), &ai.public.fingerprint()?)?;
        assert!(client.ping().await.is_err());
        server.close(0u32.into(), b"Test done");
        Ok(())
    }
    #[tokio::test]
    async fn untrusted_identity_cannot_read() -> Result<()> {
        let t = tempfile::tempdir()?;
        let b = Arc::new(Mutex::new(Core::open(t.path())?));
        let allowed = Identity::generate("allowed")?;
        let stranger = Identity::generate("stranger")?;
        let bi = Identity::generate("b")?;
        trust(
            &b.lock().unwrap(),
            &allowed.public,
            &allowed.public.fingerprint()?,
            "allowed",
            "127.0.0.1:1",
        )?;
        let server = serve(b, &bi, "127.0.0.1:0".parse()?)?;
        let peer = Peer {
            id: bi.public.fingerprint()?,
            name: "b".into(),
            address: server.local_addr()?.to_string(),
            certificate: bi.public.certificate.clone(),
        };
        let result = Client::connect(&stranger, &peer).await;
        if let Ok(client) = result {
            assert!(client.ping().await.is_err());
        }
        server.close(0u32.into(), b"Done");
        Ok(())
    }
}
