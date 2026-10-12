//! Background file indexing and authenticated receiver lifetime.
use anyhow::Result;
use horidrive_transport::{Identity, SharedCore};
use notify::{RecursiveMode, Watcher};
use std::time::Duration;

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
}
impl FileWatcher {
    pub fn start(core: SharedCore) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let root = core.lock().unwrap().root.clone();
        let event_root = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event {
                    if event.paths.iter().any(|p| {
                        p.strip_prefix(&event_root).is_ok_and(|p| {
                            !p.components()
                                .any(|c| c.as_os_str().eq_ignore_ascii_case(".horidrive"))
                        })
                    }) {
                        let _ = tx.try_send(());
                    }
                }
            })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                while rx.recv_timeout(Duration::from_millis(250)).is_ok() {}
                let mut c = core.lock().unwrap();
                match c.scan() {
                    Ok(_) => {
                        let _ = c.store.set_setting("index_error", "");
                    }
                    Err(e) => {
                        let _ = c.store.set_setting("index_error", &e.to_string());
                        tracing::warn!(
                            "File indexing incomplete; retry after resolving local I/O error"
                        );
                    }
                }
            }
        });
        Ok(Self { _watcher: watcher })
    }
}

pub async fn run(
    core: SharedCore,
    address: std::net::SocketAddr,
    interval: Option<u64>,
) -> Result<()> {
    let identity = Identity::load(&core.lock().unwrap())?;
    let _watch = FileWatcher::start(core.clone())?;
    let endpoint = horidrive_transport::serve(core.clone(), &identity, address)?;
    println!("HoriDrive receiver listening on {}", endpoint.local_addr()?);
    let worker = if let Some(seconds) = interval {
        if seconds < 2 {
            anyhow::bail!("Sync interval must be >= 2 seconds");
        }
        Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(seconds));
            loop {
                timer.tick().await;
                let peers = horidrive_transport::peers(&core.lock().unwrap()).unwrap_or_default();
                for p in peers {
                    if horidrive_transport::sync(core.clone(), &identity, &p)
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            "Peer sync failed; retry scheduled, inspect persisted job for details"
                        );
                    }
                }
            }
        }))
    } else {
        None
    };
    tokio::signal::ctrl_c().await?;
    if let Some(worker) = worker {
        worker.abort();
    }
    endpoint.close(0u32.into(), b"Shutdown");
    endpoint.wait_idle().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watcher_indexes_external_write_without_manual_scan() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let core = std::sync::Arc::new(std::sync::Mutex::new(horidrive_core::Core::open(
            tmp.path(),
        )?));
        let watcher = FileWatcher::start(core.clone())?;
        std::fs::write(tmp.path().join("watched.txt"), "live")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while core.lock().unwrap().store.entry("watched.txt")?.is_none() {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "Watcher did not index change"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        drop(watcher);
        Ok(())
    }
}
