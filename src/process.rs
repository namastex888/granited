use std::{
    fs::OpenOptions,
    io::{self, Write},
    process::Command as StdCommand,
    time::Duration,
};
use tokio::process::{Child, Command};

struct RecordedPid {
    pid: u32,
    start: String,
}

/// Owns only a spawned process and its verified native children.
/// A shared Mac slot requires individual PID records and ordered cleanup.
pub struct OwnedProcess {
    child: Child,
    registry: Option<String>,
    model: String,
    children: Vec<RecordedPid>,
}

fn start_identity(pid: u32) -> Option<String> {
    let output = StdCommand::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn signal(pid: i32, number: i32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid, number);
    }
    #[cfg(not(unix))]
    let _ = (pid, number);
}

impl OwnedProcess {
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_owned();
    }
    pub fn spawn(
        mut command: Command,
        registry: Option<String>,
        model: String,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        let process = Self {
            child,
            registry,
            model,
            children: Vec::new(),
        };
        if let Some(pid) = process.id() {
            process.record(pid, "router")?;
        }
        Ok(process)
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    fn record(&self, pid: u32, kind: &str) -> io::Result<()> {
        let Some(path) = &self.registry else {
            return Ok(());
        };
        let timestamp = |args: &[&str]| -> io::Result<String> {
            let output = StdCommand::new("date").args(args).output()?;
            if !output.status.success() {
                return Err(io::Error::other(
                    "date failed while registering an owned PID",
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let utc = timestamp(&["-u", "+%Y-%m-%dT%H:%M:%SZ"])?;
        let local = timestamp(&["+%Y-%m-%dT%H:%M:%S%z"])?;
        let cwd = std::env::current_dir()?;
        let line = format!(
            "{pid}\tgranited\t{}:{kind}\t8321\t{utc}\t{local}\tmac-m5pro\t{}\n",
            self.model,
            cwd.display()
        );
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(line.as_bytes())
    }

    pub fn register_children(&mut self) -> io::Result<()> {
        if self.registry.is_none() {
            return Ok(());
        }
        let Some(parent) = self.id() else {
            return Ok(());
        };
        let output = StdCommand::new("pgrep")
            .args(["-P", &parent.to_string(), "-x", "llama-server"])
            .output()?;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Ok(pid) = line.trim().parse::<u32>() else {
                continue;
            };
            let Some(start) = start_identity(pid) else {
                continue;
            };
            if self
                .children
                .iter()
                .any(|known| known.pid == pid && known.start == start)
            {
                continue;
            }
            self.record(pid, "model")?;
            tracing::info!(
                pid,
                parent,
                model = self.model,
                "registered owned Metal child"
            );
            self.children.push(RecordedPid { pid, start });
        }
        Ok(())
    }

    pub async fn terminate(&mut self) -> io::Result<()> {
        self.register_children()?;
        if let Some(pid) = self.id() {
            signal(
                if self.registry.is_some() {
                    pid as i32
                } else {
                    -(pid as i32)
                },
                libc::SIGTERM,
            );
        }
        if tokio::time::timeout(Duration::from_secs(8), self.child.wait())
            .await
            .is_err()
        {
            if let Some(pid) = self.id() {
                signal(
                    if self.registry.is_some() {
                        pid as i32
                    } else {
                        -(pid as i32)
                    },
                    libc::SIGKILL,
                );
            }
            self.child.kill().await?;
        }
        self.child.wait().await?;
        for known in &self.children {
            if start_identity(known.pid).as_deref() == Some(&known.start) {
                tracing::warn!(
                    pid = known.pid,
                    "recorded native child survived router shutdown; stopping that PID"
                );
                signal(known.pid as i32, libc::SIGTERM);
                for _ in 0..20 {
                    if start_identity(known.pid).as_deref() != Some(&known.start) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if start_identity(known.pid).as_deref() == Some(&known.start) {
                    signal(known.pid as i32, libc::SIGKILL);
                }
            }
        }
        Ok(())
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.register_children();
        if let Some(pid) = self.id() {
            signal(
                if self.registry.is_some() {
                    pid as i32
                } else {
                    -(pid as i32)
                },
                libc::SIGKILL,
            );
        }
        for known in &self.children {
            if start_identity(known.pid).as_deref() == Some(&known.start) {
                signal(known.pid as i32, libc::SIGKILL);
            }
        }
    }
}
