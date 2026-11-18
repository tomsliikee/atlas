//! An SSH tunnel through the system's `ssh` (OpenSSH is part of the GNOME
//! runtime). Using the real client means `~/.ssh/config` (aliases,
//! ProxyJump, ports), the agent and `known_hosts` behave exactly as in a
//! terminal. Atlas never sees a key or a passphrase: with `BatchMode` the
//! agent must hold the key, and an unknown host key is refused.

use std::process::Stdio;
use std::time::Duration;

use atlas_core::store::Ssh;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

pub struct Tunnel {
    pub local_port: u16,
    _child: Child,
}

impl Tunnel {
    pub async fn open(ssh: &Ssh, remote_host: &str, remote_port: u16) -> Result<Tunnel, String> {
        // A free local port; the tiny window between closing it and ssh
        // binding it is acceptable on a desktop.
        let local_port = {
            let probe = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
            probe.local_addr().map_err(|e| e.to_string())?.port()
        };
        let mut cmd = Command::new("ssh");
        cmd.arg("-N")
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ConnectTimeout=10"])
            .args(["-o", "ServerAliveInterval=30"])
            .arg("-L")
            .arg(format!("127.0.0.1:{local_port}:{remote_host}:{remote_port}"));
        if let Some(port) = ssh.port {
            cmd.arg("-p").arg(port.to_string());
        }
        if let Some(user) = ssh.user.as_deref().filter(|u| !u.is_empty()) {
            cmd.arg("-l").arg(user);
        }
        // `--` so a host field can never be read as an option.
        cmd.arg("--").arg(&ssh.host);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("ssh: {e}"))?;

        // Wait until the forward accepts connections, or ssh gives up.
        for _ in 0..150 {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err).await;
                }
                let err = err.trim();
                return Err(if err.is_empty() { format!("ssh ended ({status})") } else { format!("ssh: {err}") });
            }
            if TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
                return Ok(Tunnel { local_port, _child: child });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("ssh: the tunnel did not open within 15 s".into())
    }
}
