use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver};
use portable_pty::{CommandBuilder, PtySize, native_pty_system, MasterPty};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

/// Owns a spawned child process attached to a pseudo-terminal.
///
/// The reader runs on a dedicated OS thread that pushes byte chunks onto a
/// channel; the UI drains that channel from its refresh loop. Writes
/// (keystrokes) are issued synchronously on the master writer.
pub struct PtySession {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    output_rx: Receiver<Vec<u8>>,
}

impl PtySession {
    pub fn spawn_shell(rows: u16, cols: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };
        let pair = pty_system
            .openpty(size)
            .context("openpty failed")?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let _child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        // The slave is no longer needed on our side; dropping it closes the
        // slave fd so the child only has the pty as its controlling terminal.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("clone pty reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("take pty writer")?;

        let (tx, rx) = unbounded::<Vec<u8>>();
        thread::Builder::new()
            .name("cuartel-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            log::warn!("pty read error: {e}");
                            break;
                        }
                    }
                }
            })
            .context("spawn pty reader thread")?;

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(pair.master)),
            output_rx: rx,
        })
    }

    pub fn drain_output(&self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.output_rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            if let Err(e) = w.write_all(bytes) {
                log::warn!("pty write failed: {e}");
            }
            let _ = w.flush();
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

}
