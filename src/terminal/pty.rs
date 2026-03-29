/// PTY session management using portable-pty
///
/// Handles spawning and managing the PTY process
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use std::io::{Read, Write};

/// PTY session configuration
pub struct PtyConfig {
    pub cols: u16,
    pub rows: u16,
    pub shell: Option<String>,
    pub term: String,
}

/// A PTY session
pub struct PtySession {
    pub pty_pair: PtyPair,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Kill the child process to prevent zombies
        if let Some(pid) = self.child.process_id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        // Reap the child
        let _ = self.child.wait();
    }
}

impl PtySession {
    /// Spawn a new PTY session
    pub fn spawn(config: PtyConfig) -> Result<Self> {
        let pty_system = NativePtySystem::default();

        let pty_size = PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty_pair = pty_system.openpty(pty_size).context("Failed to open PTY")?;

        let shell = config
            .shell
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()));

        let mut cmd = CommandBuilder::new(&shell);
        cmd.env("TERM", config.term);

        let child = pty_pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn shell")?;

        let writer = pty_pair
            .master
            .take_writer()
            .context("Failed to take PTY writer")?;

        Ok(PtySession {
            pty_pair,
            child,
            writer,
        })
    }

    /// Write input to the PTY
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Get a clone of the reader for async operations
    pub fn clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        self.pty_pair
            .master
            .try_clone_reader()
            .context("Failed to clone PTY reader")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_with_explicit_shell() {
        let config = PtyConfig {
            cols: 80,
            rows: 24,
            shell: Some("/bin/sh".to_string()),
            term: "xterm".to_string(),
        };
        let session = PtySession::spawn(config);
        assert!(
            session.is_ok(),
            "PtySession::spawn with /bin/sh should succeed"
        );
    }

    #[test]
    fn test_write_and_read_back() {
        let config = PtyConfig {
            cols: 80,
            rows: 24,
            shell: Some("/bin/echo".to_string()),
            term: "xterm".to_string(),
        };
        let session = PtySession::spawn(config).unwrap();
        let mut reader = session.clone_reader().unwrap();

        // /bin/echo prints its arguments (none here) followed by newline, then exits
        // Give it a moment to produce output
        std::thread::sleep(std::time::Duration::from_millis(200));

        let mut buf = [0u8; 256];
        let n = reader.read(&mut buf).unwrap_or(0);
        // echo with no args produces at least a newline
        assert!(n > 0, "Should read output from /bin/echo");
    }

    #[test]
    fn test_clone_reader_works() {
        let config = PtyConfig {
            cols: 80,
            rows: 24,
            shell: Some("/bin/sh".to_string()),
            term: "xterm".to_string(),
        };
        let session = PtySession::spawn(config).unwrap();
        let reader1 = session.clone_reader();
        assert!(reader1.is_ok(), "First clone_reader should succeed");
        let reader2 = session.clone_reader();
        assert!(reader2.is_ok(), "Second clone_reader should also succeed");
    }
}
