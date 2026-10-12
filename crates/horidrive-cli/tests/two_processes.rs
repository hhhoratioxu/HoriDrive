//! Real CLI processes, OS credentials, UDP and on-disk state; no mock server.
#![cfg(windows)]
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
const BIN: &str = env!("CARGO_BIN_EXE_horidrive");
fn run(root: &Path, args: &[&str]) -> Result<Value> {
    let out = Command::new(BIN)
        .arg("--root")
        .arg(root)
        .args(args)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("CLI failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    if out.stdout.is_empty() {
        Ok(Value::Null)
    } else {
        Ok(serde_json::from_slice(&out.stdout)?)
    }
}
struct Receiver(Child);
impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn serve(root: &Path, address: &str) -> Result<Receiver> {
    let mut child = Command::new(BIN)
        .arg("--root")
        .arg(root)
        .args(["serve", "--listen", address])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = child.stdout.take().context("Missing stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let receiver = Receiver(child);
    let line = rx.recv_timeout(Duration::from_secs(15))??;
    anyhow::ensure!(
        line.contains("receiver listening"),
        "Node failed to start: {line}"
    );
    Ok(receiver)
}
struct Credentials(Vec<String>);
impl Drop for Credentials {
    fn drop(&mut self) {
        for id in &self.0 {
            if let Ok(e) = keyring::Entry::new("HoriDrive", id) {
                let _ = e.delete_credential();
            }
        }
    }
}
fn unused_port() -> Result<String> {
    Ok(std::net::UdpSocket::bind("127.0.0.1:0")?
        .local_addr()?
        .to_string())
}

#[test]
fn independent_nodes_sync_conflicts_and_resume_after_receiver_crash() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let pa = tmp.path().join("a.json");
    let pb = tmp.path().join("b.json");
    let ai = run(&a, &["identity", pa.to_str().unwrap()])?;
    let bi = run(&b, &["identity", pb.to_str().unwrap()])?;
    let _credentials = Credentials(vec![
        ai["id"].as_str().unwrap().into(),
        bi["id"].as_str().unwrap().into(),
    ]);
    let af = ai["fingerprint"].as_str().unwrap();
    let bf = bi["fingerprint"].as_str().unwrap();
    let aa = unused_port()?;
    let ba = unused_port()?;
    run(
        &a,
        &[
            "trust",
            pb.to_str().unwrap(),
            "--fingerprint",
            bf,
            "--address",
            &ba,
            "--name",
            "B",
        ],
    )?;
    run(
        &b,
        &[
            "trust",
            pa.to_str().unwrap(),
            "--fingerprint",
            af,
            "--address",
            &aa,
            "--name",
            "A",
        ],
    )?;
    std::fs::create_dir(a.join("目录"))?;
    std::fs::write(a.join("目录/中文 #+.txt"), "initial")?;
    std::fs::write(a.join("empty.txt"), [])?;
    let receiver = serve(&b, &ba)?;
    run(&a, &["sync", bf])?;
    assert_eq!(
        std::fs::read_to_string(b.join("目录/中文 #+.txt"))?,
        "initial"
    );
    assert_eq!(std::fs::metadata(b.join("empty.txt"))?.len(), 0);
    std::fs::write(a.join("目录/中文 #+.txt"), "edited on A")?;
    std::fs::write(b.join("目录/中文 #+.txt"), "edited on B")?;
    run(&a, &["sync", bf])?;
    assert_eq!(
        std::fs::read(a.join("目录/中文 #+.txt"))?,
        std::fs::read(b.join("目录/中文 #+.txt"))?
    );
    assert!(std::fs::read_dir(a.join("目录"))?.any(|f| f
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("conflict")));
    run(&a, &["delete", "empty.txt"])?;
    run(&a, &["sync", bf])?;
    assert!(!b.join("empty.txt").exists());
    drop(receiver); // Abrupt receiver termination, then re-open the same persisted index.
    run(
        &b,
        &[
            "quota",
            &((horidrive_storage::BLOCK_SIZE + 1024) as u64).to_string(),
        ],
    )?;
    let mut f = std::fs::File::create(a.join("large.bin"))?;
    f.write_all(&vec![17; horidrive_storage::BLOCK_SIZE])?;
    f.write_all(&vec![23; horidrive_storage::BLOCK_SIZE])?;
    drop(f);
    let receiver = serve(&b, &ba)?;
    let failed = Command::new(BIN)
        .arg("--root")
        .arg(&a)
        .args(["sync", bf])
        .output()?;
    assert!(!failed.status.success());
    let blocks = std::fs::read_dir(b.join(".horidrive/blocks"))?.count();
    assert!(blocks > 0);
    drop(receiver);
    run(&b, &["quota", "0"])?;
    let receiver = serve(&b, &ba)?;
    let resumed = run(&a, &["sync", bf])?;
    assert!(
        resumed["bytes"].as_u64().unwrap() < 2 * horidrive_storage::BLOCK_SIZE as u64,
        "Verified uploaded blocks must not be resent"
    );
    assert_eq!(
        std::fs::read(a.join("large.bin"))?,
        std::fs::read(b.join("large.bin"))?
    );
    drop(receiver);
    let bstate = run(&b, &["scan"])?;
    assert!(bstate
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["path"] == "empty.txt" && e["deleted"] == true));
    for _ in 0..3 {
        run(&b, &["info"])?;
    }
    let receiver = serve(&b, &ba)?;
    let dest = a.join("download.bin");
    let manifest = run(&a, &["remote-ls", bf])?;
    assert!(manifest
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "large.bin"));
    // Fetch a remote file through the CLI's cache path on a fresh local filename.
    std::fs::write(b.join("ondemand.txt"), "cache me")?;
    run(&a, &["fetch", bf, "ondemand.txt"])?;
    assert_eq!(std::fs::read_to_string(a.join("ondemand.txt"))?, "cache me");
    run(&a, &["export", "large.bin", dest.to_str().unwrap()])?;
    // Crash sender and downloader after the first verified block, during a rate-limited transfer.
    let mut remote = std::fs::File::create(b.join("network.bin"))?;
    for value in [101u8, 102, 103] {
        remote.write_all(&vec![value; horidrive_storage::BLOCK_SIZE])?;
    }
    drop(remote);
    run(&a, &["rate-limit", "1048576"])?;
    let downloader = Receiver(
        Command::new(BIN)
            .arg("--root")
            .arg(&a)
            .args(["sync", bf])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let first = blake3::hash(&vec![101; horidrive_storage::BLOCK_SIZE])
        .to_hex()
        .to_string();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !a.join(".horidrive/blocks").join(&first).exists() {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "First block was not received"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(receiver);
    drop(downloader);
    assert!(!a.join("network.bin").exists());
    run(&a, &["rate-limit", "0"])?;
    let receiver = serve(&b, &ba)?;
    let job = run(&a, &["sync", bf])?;
    assert_eq!(
        job["bytes"].as_u64(),
        Some(2 * horidrive_storage::BLOCK_SIZE as u64)
    );
    assert_eq!(
        std::fs::read(a.join("network.bin"))?,
        std::fs::read(b.join("network.bin"))?
    );
    drop(receiver);
    assert!(dest.exists());
    Ok(())
}
