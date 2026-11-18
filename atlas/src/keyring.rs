//! Passwords in the desktop keyring, only for connections where the user
//! turned "Remember password in the keyring" on. In the Flatpak oo7 uses the
//! Secret portal (a file keyring inside the sandbox, keyed by the portal);
//! outside it talks to the Secret Service. Atlas' own files never hold a
//! password either way.

const APP: &str = "io.github.tomsliikee.Atlas";

fn attributes(id: &str) -> [(&'static str, String); 2] {
    [("application", APP.to_owned()), ("connection", id.to_owned())]
}

async fn keyring() -> Result<oo7::Keyring, String> {
    oo7::Keyring::new().await.map_err(|e| e.to_string())
}

/// The stored password of connection `id`, if any.
pub async fn load(id: String) -> Option<String> {
    let k = keyring().await.ok()?;
    let _ = k.unlock().await;
    let items = k.search_items(&attributes(&id)).await.ok()?;
    let secret = items.first()?.secret().await.ok()?;
    String::from_utf8(secret.as_bytes().to_vec()).ok()
}

pub async fn store(id: String, label: String, password: String) -> Result<(), String> {
    let k = keyring().await?;
    let _ = k.unlock().await;
    k.create_item(&format!("Atlas: {label}"), &attributes(&id), password, true).await.map_err(|e| e.to_string())
}

pub async fn forget(id: String) {
    if let Ok(k) = keyring().await {
        let _ = k.delete(&attributes(&id)).await;
    }
}
