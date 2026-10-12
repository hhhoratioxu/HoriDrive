#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use horidrive_core::Core;
use horidrive_transport::{Client, Identity, Receiver, SharedCore};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct AppState {
    core: SharedCore,
    identity: Mutex<Option<Identity>>,
    receiver: Mutex<Option<Receiver>>,
    _watch: Mutex<horidrive_daemon::FileWatcher>,
}
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing argument: {key}"))
}
fn number(args: &Value, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Invalid number: {key}"))
}
impl AppState {
    fn identity(&self) -> Result<Identity, String> {
        let mut identity = self.identity.lock().unwrap();
        if identity.is_none() {
            *identity =
                Some(Identity::load(&self.core.lock().unwrap()).map_err(|e| e.to_string())?);
        }
        Ok(identity.as_ref().unwrap().clone())
    }
    fn stop(&self) {
        if let Some(endpoint) = self.receiver.lock().unwrap().take() {
            endpoint.close(0u32.into(), b"Receiver stopped");
        }
    }
}
#[tauri::command]
async fn request(
    state: tauri::State<'_, AppState>,
    op: String,
    args: Value,
) -> Result<Value, String> {
    if [
        "listen",
        "stop",
        "revoke",
        "identity_export",
        "identity_inspect",
        "trust",
        "sync",
        "probe",
        "remote_list",
        "remote_fetch",
    ]
    .contains(&op.as_str())
    {
        match op.as_str() {
            "stop" => {
                state.stop();
                return Ok(Value::Null);
            }
            "identity_export" => {
                let identity = state.identity()?;
                std::fs::write(
                    text(&args, "dest")?,
                    serde_json::to_vec_pretty(&identity.public).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                return Ok(
                    json!({"fingerprint":identity.public.fingerprint().map_err(|e|e.to_string())?}),
                );
            }
            "identity_inspect" => {
                let p = horidrive_transport::read_public(&PathBuf::from(text(&args, "source")?))
                    .map_err(|e| e.to_string())?;
                return Ok(
                    json!({"id":p.id,"fingerprint":p.fingerprint().map_err(|e|e.to_string())?}),
                );
            }
            "trust" => {
                let p = horidrive_transport::read_public(&PathBuf::from(text(&args, "source")?))
                    .map_err(|e| e.to_string())?;
                let peer = horidrive_transport::trust(
                    &state.core.lock().unwrap(),
                    &p,
                    text(&args, "fingerprint")?,
                    text(&args, "name")?,
                    text(&args, "address")?,
                )
                .map_err(|e| e.to_string())?;
                state.stop();
                return Ok(json!(peer));
            }
            "revoke" => {
                state.stop();
                horidrive_transport::revoke(&state.core.lock().unwrap(), text(&args, "peer")?)
                    .map_err(|e| e.to_string())?;
                return Ok(Value::Null);
            }
            "listen" => {
                let identity = state.identity()?;
                let address = text(&args, "address")?
                    .parse()
                    .map_err(|_| "Expected IP:port".to_string())?;
                state.stop();
                let endpoint = horidrive_transport::serve(state.core.clone(), &identity, address)
                    .map_err(|e| e.to_string())?;
                *state.receiver.lock().unwrap() = Some(endpoint);
                return Ok(Value::Null);
            }
            _ => {
                let identity = state.identity()?;
                let peer = horidrive_transport::peers(&state.core.lock().unwrap())
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|p| Some(p.id.as_str()) == args.get("peer").and_then(Value::as_str))
                    .ok_or("Trusted device not found")?;
                if op == "sync" {
                    return horidrive_transport::sync(state.core.clone(), &identity, &peer)
                        .await
                        .map(|j| json!(j))
                        .map_err(|e| e.to_string());
                }
                if op == "remote_fetch" {
                    return horidrive_transport::fetch(
                        state.core.clone(),
                        &identity,
                        &peer,
                        text(&args, "path")?,
                    )
                    .await
                    .map(|e| json!(e))
                    .map_err(|e| e.to_string());
                }
                let client = Client::connect(&identity, &peer)
                    .await
                    .map_err(|e| e.to_string())?;
                return if op == "probe" {
                    client.ping().await
                } else {
                    client.list(text(&args, "path")?).await
                }
                .map_err(|e| e.to_string());
            }
        }
    }
    let core = state.core.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let action = || -> anyhow::Result<Value> {
            let mut c = core.lock().unwrap();
            let t = |key: &str| text(&args, key).map_err(anyhow::Error::msg);
            match op.as_str() {
                "overview" => Ok(json!(c.overview()?)),
                "list" => Ok(json!(c.list(t("path")?)?)),
                "search" => Ok(json!(c.search(t("query")?)?)),
                "recent" => Ok(json!(c.recent()?)),
                "scan" => Ok(json!(c.scan()?)),
                "mkdir" => {
                    c.mkdir(t("path")?)?;
                    Ok(Value::Null)
                }
                "import" => {
                    c.import(&PathBuf::from(t("source")?), t("path")?)?;
                    Ok(Value::Null)
                }
                "export" => {
                    c.export(t("path")?, &PathBuf::from(t("dest")?))?;
                    Ok(Value::Null)
                }
                "copy" => {
                    c.copy(t("from")?, t("to")?)?;
                    Ok(Value::Null)
                }
                "move" => {
                    c.move_file(t("from")?, t("to")?)?;
                    Ok(Value::Null)
                }
                "delete" => Ok(json!(c.delete(t("path")?)?)),
                "trash" => Ok(json!(c.trash()?)),
                "restore_trash" => {
                    c.restore_trash(t("id")?)?;
                    Ok(Value::Null)
                }
                "versions" => Ok(json!(c.versions(t("path")?)?)),
                "restore_version" => {
                    c.restore_version(
                        t("path")?,
                        number(&args, "id").map_err(anyhow::Error::msg)? as i64,
                    )?;
                    Ok(Value::Null)
                }
                "preview" => Ok(json!(c.preview(t("path")?)?)),
                "quota" => {
                    c.store
                        .set_quota(number(&args, "bytes").map_err(anyhow::Error::msg)?)?;
                    Ok(Value::Null)
                }
                "prune" => Ok(json!(c.store.prune()?)),
                "peers" => Ok(json!(horidrive_transport::peers(&c)?)),
                "jobs" => Ok(json!(c.store.jobs()?)),
                "discover" => Ok(json!(horidrive_transport::discovery::browse(
                    std::time::Duration::from_secs(3)
                )?)),
                "cancel" => {
                    horidrive_transport::cancel(&c, t("id")?)?;
                    Ok(Value::Null)
                }
                "rate_limit" => {
                    c.store.set_setting(
                        "rate_limit_bps",
                        &number(&args, "bytes")
                            .map_err(anyhow::Error::msg)?
                            .to_string(),
                    )?;
                    Ok(Value::Null)
                }
                _ => anyhow::bail!("Unknown operation"),
            }
        };
        action().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::Manager;
            let root = std::env::var_os("HORIDRIVE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(
                        std::env::var_os("USERPROFILE")
                            .or_else(|| std::env::var_os("HOME"))
                            .unwrap_or_else(|| ".".into()),
                    )
                    .join("HoriDrive Files")
                });
            let core = Arc::new(Mutex::new(Core::open(&root)?));
            let watch = horidrive_daemon::FileWatcher::start(core.clone())?;
            app.manage(AppState {
                core,
                identity: Mutex::new(None),
                receiver: Mutex::new(None),
                _watch: Mutex::new(watch),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![request])
        .run(tauri::generate_context!())
        .expect("HoriDrive failed to start");
}
