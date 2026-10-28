use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
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
    /// `state_dir/owned/<model>.pid`: this worker's PID and start time while it runs. A manager
    /// killed outright leaves it behind, and the next manager on the same state directory (which
    /// holds that directory's exclusive lock) stops exactly that process group, and nothing else.
    ledger: Option<PathBuf>,
    /// The deployment's `slot_lock`, held while this process is owned; dropping it releases the lock.
    slot: Option<std::fs::File>,
}

/// Stop the workers a previous manager on this state directory left running when it was killed:
/// only processes whose PID and start time match a ledger entry, so a reused PID is never touched.
/// Returns one line per entry for the log.
pub fn reap_orphans(dir: &Path) -> Vec<String> {
    let mut report = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return report;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut fields = text.trim_end().splitn(2, '\t');
        let pid = fields.next().and_then(|p| p.parse::<u32>().ok());
        let start = fields.next().map(str::to_owned);
        let name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match (pid, start) {
            (Some(pid), Some(start)) if start_identity(pid).as_deref() == Some(start.as_str()) => {
                signal(-(pid as i32), libc::SIGTERM);
                for _ in 0..50 {
                    if start_identity(pid).as_deref() != Some(start.as_str()) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if start_identity(pid).as_deref() == Some(start.as_str()) {
                    signal(-(pid as i32), libc::SIGKILL);
                }
                report.push(format!("{name}: stopped orphaned worker group {pid}"));
            }
            _ => report.push(format!("{name}: stale entry; process already gone")),
        }
        let _ = std::fs::remove_file(&path);
    }
    report
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
        ledger: Option<PathBuf>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        let mut process = Self {
            child,
            registry,
            model,
            children: Vec::new(),
            ledger: None,
            slot: None,
        };
        if let Some(pid) = process.id() {
            process.record(pid, "router")?;
            if let Some(path) = ledger {
                let start = start_identity(pid)
                    .ok_or_else(|| io::Error::other("spawned worker has no start time"))?;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, format!("{pid}\t{start}\n"))?;
                process.ledger = Some(path);
            }
        }
        Ok(process)
    }

    fn clear_ledger(&mut self) {
        if let Some(path) = self.ledger.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub fn hold_slot(&mut self, slot: std::fs::File) {
        self.slot = Some(slot);
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
        self.clear_ledger();
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
        self.clear_ledger();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reaping_stops_only_the_recorded_process() {
        let dir = std::env::temp_dir().join(format!("granited-reap-{}", uuid::Uuid::new_v4()));
        let mut command = Command::new("sleep");
        command.arg("30");
        let worker =
            OwnedProcess::spawn(command, None, "m".into(), Some(dir.join("m.pid"))).unwrap();
        let pid = worker.id().unwrap();
        let start = start_identity(pid).unwrap();
        // A manager killed outright never runs Drop: the worker and its ledger entry survive it.
        std::mem::forget(worker);
        assert_eq!(
            std::fs::read_to_string(dir.join("m.pid")).unwrap(),
            format!("{pid}\t{start}\n")
        );
        // An entry whose start time differs (a reused PID) is dropped without a signal.
        std::fs::rename(dir.join("m.pid"), dir.join("held.pid")).unwrap();
        std::fs::write(
            dir.join("other.pid"),
            format!("{pid}\tMon Jan  1 00:00:00 2001\n"),
        )
        .unwrap();
        std::fs::remove_file(dir.join("held.pid")).unwrap();
        let report = reap_orphans(&dir);
        assert_eq!(
            report,
            vec!["other: stale entry; process already gone".to_owned()]
        );
        assert_eq!(start_identity(pid).as_deref(), Some(start.as_str()));
        // The matching entry stops exactly that process group.
        std::fs::write(dir.join("m.pid"), format!("{pid}\t{start}\n")).unwrap();
        assert_eq!(
            reap_orphans(&dir),
            vec![format!("m: stopped orphaned worker group {pid}")]
        );
        // In a real crash init adopts and collects the orphan; here the test is its parent.
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid as i32, &mut status, 0) },
            pid as i32
        );
        assert!(
            libc::WIFSIGNALED(status),
            "the recorded worker was stopped by a signal"
        );
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "every entry is removed"
        );
        let _ = std::fs::remove_dir(&dir);
    }
}
