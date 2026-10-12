use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use horidrive_core::Core;
use horidrive_transport::{Client, Identity, PublicIdentity};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Parser)]
#[command(
    name = "horidrive",
    version,
    about = "HoriDrive — Your Cloud. Your Rules."
)]
struct Cli {
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Info,
    Scan,
    Ls {
        #[arg(default_value = "")]
        path: String,
    },
    Search {
        query: String,
    },
    Mkdir {
        path: String,
    },
    Import {
        source: PathBuf,
        path: String,
    },
    Export {
        path: String,
        dest: PathBuf,
    },
    Copy {
        from: String,
        to: String,
    },
    Move {
        from: String,
        to: String,
    },
    Delete {
        path: String,
    },
    Trash,
    Restore {
        id: String,
    },
    Versions {
        path: String,
    },
    RestoreVersion {
        path: String,
        id: i64,
    },
    Quota {
        bytes: u64,
    },
    Prune,
    RateLimit {
        bytes: u64,
    },
    Cancel {
        id: String,
    },
    /// Export a PUBLIC certificate; the private key stays in the OS credential store.
    Identity {
        output: Option<PathBuf>,
    },
    /// Trust only after checking the full fingerprint independently. Repeat on the other device.
    Trust {
        certificate: PathBuf,
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "My device")]
        name: String,
    },
    Peers,
    Discover,
    Revoke {
        peer: String,
    },
    Probe {
        peer: String,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:44330")]
        listen: std::net::SocketAddr,
        #[arg(long)]
        interval: Option<u64>,
    },
    Sync {
        peer: String,
    },
    RemoteLs {
        peer: String,
        #[arg(default_value = "")]
        path: String,
    },
    /// Download one remote file into local cache (never reports an offline node as online).
    Fetch {
        peer: String,
        path: String,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let root = cli.root.unwrap_or_else(|| {
        PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap_or_else(|| ".".into()),
        )
        .join("HoriDrive Files")
    });
    let core = Arc::new(Mutex::new(Core::open(&root)?));
    macro_rules! json {
        ($value:expr) => {
            println!("{}", serde_json::to_string_pretty(&$value)?)
        };
    }
    macro_rules! c {
        () => {
            core.lock().unwrap()
        };
    }
    match cli.command {
        Command::Info => json!(c!().overview()?),
        Command::Scan => json!(c!().scan()?),
        Command::Ls { path } => json!(c!().list(&path)?),
        Command::Search { query } => json!(c!().search(&query)?),
        Command::Mkdir { path } => c!().mkdir(&path)?,
        Command::Import { source, path } => c!().import(&source, &path)?,
        Command::Export { path, dest } => c!().export(&path, &dest)?,
        Command::Copy { from, to } => c!().copy(&from, &to)?,
        Command::Move { from, to } => c!().move_file(&from, &to)?,
        Command::Delete { path } => json!(c!().delete(&path)?),
        Command::Trash => json!(c!().trash()?),
        Command::Restore { id } => c!().restore_trash(&id)?,
        Command::Versions { path } => json!(c!().versions(&path)?),
        Command::RestoreVersion { path, id } => c!().restore_version(&path, id)?,
        Command::Quota { bytes } => c!().store.set_quota(bytes)?,
        Command::Prune => json!(c!().store.prune()?),
        Command::RateLimit { bytes } => c!()
            .store
            .set_setting("rate_limit_bps", &bytes.to_string())?,
        Command::Cancel { id } => horidrive_transport::cancel(&c!(), &id)?,
        Command::Identity { output } => {
            let identity = Identity::load(&c!())?;
            if let Some(output) = output {
                std::fs::write(output, serde_json::to_vec_pretty(&identity.public)?)?;
            }
            json!(
                serde_json::json!({"id":identity.public.id,"fingerprint":identity.public.fingerprint()?})
            );
        }
        Command::Trust {
            certificate,
            fingerprint,
            address,
            name,
        } => {
            let public: PublicIdentity = horidrive_transport::read_public(&certificate)?;
            json!(horidrive_transport::trust(
                &c!(),
                &public,
                &fingerprint,
                &name,
                &address
            )?);
        }
        Command::Peers => json!(horidrive_transport::peers(&c!())?),
        Command::Discover => json!(horidrive_transport::discovery::browse(
            std::time::Duration::from_secs(3)
        )?),
        Command::Revoke { peer } => horidrive_transport::revoke(&c!(), &peer)?,
        Command::Serve { listen, interval } => {
            horidrive_daemon::run(core.clone(), listen, interval).await?
        }
        command => {
            let peer_id = match &command {
                Command::Sync { peer }
                | Command::Probe { peer }
                | Command::RemoteLs { peer, .. }
                | Command::Fetch { peer, .. } => peer,
                _ => unreachable!(),
            };
            let identity = Identity::load(&c!())?;
            let peer = horidrive_transport::peers(&c!())?
                .into_iter()
                .find(|p| &p.id == peer_id)
                .context("Trusted device not found; use 'peers' to find its fingerprint")?;
            if let Command::Sync { .. } = command {
                json!(horidrive_transport::sync(core.clone(), &identity, &peer).await?);
            } else {
                let client = Client::connect(&identity, &peer).await?;
                match command {
                    Command::Probe { .. } => json!(client.ping().await?),
                    Command::RemoteLs { path, .. } => json!(client.list(&path).await?),
                    Command::Fetch { path, .. } => {
                        let e = client
                            .manifest()
                            .await?
                            .into_iter()
                            .find(|e| e.path == path && !e.deleted && !e.directory)
                            .context("Remote file not found")?;
                        for h in &e.blocks {
                            if !c!().store.has_block(h)? {
                                let data = client.get(h).await?;
                                c!().store.put_block(h, &data)?;
                            }
                        }
                        c!().apply(&e)?;
                        json!(e);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
    Ok(())
}
