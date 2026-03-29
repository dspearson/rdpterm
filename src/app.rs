/// Terminal application implementing RdpApplication
///
/// Bridges the RDP framebuffer server with terminal emulation, PTY management,
/// and font rendering.
use anyhow::{Context, Result};
use rdpfb::application::{RdpApplication, RdpApplicationFactory, RdpAuthenticator};
use rdpfb::framebuffer::Framebuffer;
use rdpfb::protocol::pdu::{KBD_FLAG_EXTENDED, KBD_FLAG_RELEASE};
use rdpfb::protocol::rdp::InputEvent;
use std::io::Read;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::debug;

use crate::terminal::{PtyConfig, PtySession, RendererConfig, TerminalEmulator, TerminalRenderer};

/// Terminal application that renders a PTY session into an RDP framebuffer.
pub struct TerminalApp {
    shell: Option<String>,
    pty: Option<PtySession>,
    emulator: Option<TerminalEmulator>,
    renderer: TerminalRenderer,
    // Modifier key state
    shift_pressed: bool,
    ctrl_pressed: bool,
    alt_pressed: bool,
    // Content notification
    content_notify: Arc<Notify>,
    /// Handle for the PTY reader task (kept alive while the app exists)
    _pty_reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Channel receiver for PTY output data
    pty_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
}

impl TerminalApp {
    pub fn new(shell: Option<String>, font_size: f32) -> Result<Self> {
        let renderer_config = RendererConfig { font_size };
        let renderer =
            TerminalRenderer::new(renderer_config).context("Failed to create terminal renderer")?;

        Ok(TerminalApp {
            shell,
            pty: None,
            emulator: None,
            renderer,
            shift_pressed: false,
            ctrl_pressed: false,
            alt_pressed: false,
            content_notify: Arc::new(Notify::new()),
            _pty_reader_handle: None,
            pty_rx: None,
        })
    }

    fn pty(&mut self) -> &mut PtySession {
        self.pty
            .as_mut()
            .expect("PTY not initialised — call after on_connect")
    }

    fn emu(&mut self) -> &mut TerminalEmulator {
        self.emulator
            .as_mut()
            .expect("Emulator not initialised — call after on_connect")
    }
}

impl RdpApplication for TerminalApp {
    fn on_connect(&mut self, width: u16, height: u16, _framebuffer: &mut Framebuffer) -> Result<()> {
        let cols = (width as f32 / self.renderer.cell_width) as u16;
        let rows = (height as f32 / self.renderer.cell_height) as u16;
        tracing::info!(
            "Spawning terminal: {} cols x {} rows (cell: {:.1}x{:.1}px, screen: {}x{})",
            cols,
            rows,
            self.renderer.cell_width,
            self.renderer.cell_height,
            width,
            height
        );

        let pty_config = PtyConfig {
            cols,
            rows,
            shell: self.shell.clone(),
            term: "xterm-256color".to_string(),
        };
        self.pty = Some(PtySession::spawn(pty_config).context("Failed to spawn PTY session")?);
        self.emulator = Some(
            TerminalEmulator::new(cols as usize, rows as usize)
                .context("Failed to create terminal emulator")?,
        );

        // Start PTY reader thread that signals content_notify
        let pty_reader = self
            .pty()
            .clone_reader()
            .context("Failed to clone PTY reader")?;
        let notify = self.content_notify.clone();

        let (pty_tx, pty_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        self._pty_reader_handle = Some(tokio::task::spawn_blocking(move || {
            let mut reader = pty_reader;
            let mut buf = vec![0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        debug!("PTY reader: EOF — PTY closed");
                        break;
                    }
                    Ok(n) => {
                        if pty_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                        notify.notify_one();
                    }
                    Err(e) => {
                        debug!("PTY read error: {}", e);
                        break;
                    }
                }
            }
        }));

        self.pty_rx = Some(pty_rx);

        Ok(())
    }

    fn on_input(&mut self, event: InputEvent) -> Result<()> {
        match event {
            InputEvent::Mouse { .. } => {
                // Mouse events are ignored unless the terminal application has
                // enabled mouse tracking (e.g. vim, tmux via \e[?1000h).
                // We don't track that state yet, so silently drop all mouse events
                // to prevent junk escape sequences appearing in the shell.
            }
            InputEvent::Scancode { flags, scancode } => {
                let is_release = (flags & KBD_FLAG_RELEASE) != 0;
                let is_extended = (flags & KBD_FLAG_EXTENDED) != 0;

                // Track modifier state
                match scancode {
                    0x2A | 0x36 => {
                        self.shift_pressed = !is_release;
                        return Ok(());
                    }
                    0x1D => {
                        self.ctrl_pressed = !is_release;
                        return Ok(());
                    }
                    0x38 => {
                        self.alt_pressed = !is_release;
                        return Ok(());
                    }
                    _ => {}
                }

                // Only act on key press, not release
                if is_release {
                    return Ok(());
                }

                // Ctrl+key combos
                if self.ctrl_pressed {
                    if let Some(ch) = InputEvent::scancode_to_char(scancode, false) {
                        let ctrl_byte = (ch as u8).wrapping_sub(b'a').wrapping_add(1);
                        if (1..=26).contains(&ctrl_byte) {
                            self.pty().write(&[ctrl_byte])?;
                            return Ok(());
                        }
                    }
                    return Ok(());
                }

                // Extended keys (arrows, home, end, etc.)
                if is_extended {
                    let escape_seq: Option<&[u8]> = match scancode {
                        0x48 => Some(b"\x1b[A"),  // Up
                        0x50 => Some(b"\x1b[B"),  // Down
                        0x4D => Some(b"\x1b[C"),  // Right
                        0x4B => Some(b"\x1b[D"),  // Left
                        0x47 => Some(b"\x1b[H"),  // Home
                        0x4F => Some(b"\x1b[F"),  // End
                        0x49 => Some(b"\x1b[5~"), // Page Up
                        0x51 => Some(b"\x1b[6~"), // Page Down
                        0x52 => Some(b"\x1b[2~"), // Insert
                        0x53 => Some(b"\x1b[3~"), // Delete
                        _ => None,
                    };
                    if let Some(seq) = escape_seq {
                        self.pty().write(seq)?;
                        return Ok(());
                    }
                    return Ok(());
                }

                // Non-extended special keys
                match scancode {
                    0x01 => {
                        self.pty().write(b"\x1b")?;
                    } // Escape
                    0x0E => {
                        self.pty().write(b"\x7f")?;
                    } // Backspace (DEL)
                    0x0F => {
                        self.pty().write(b"\t")?;
                    } // Tab
                    0x1C => {
                        self.pty().write(b"\r")?;
                    } // Enter (CR)
                    // Function keys
                    0x3B => {
                        self.pty().write(b"\x1bOP")?;
                    } // F1
                    0x3C => {
                        self.pty().write(b"\x1bOQ")?;
                    } // F2
                    0x3D => {
                        self.pty().write(b"\x1bOR")?;
                    } // F3
                    0x3E => {
                        self.pty().write(b"\x1bOS")?;
                    } // F4
                    0x3F => {
                        self.pty().write(b"\x1b[15~")?;
                    } // F5
                    0x40 => {
                        self.pty().write(b"\x1b[17~")?;
                    } // F6
                    0x41 => {
                        self.pty().write(b"\x1b[18~")?;
                    } // F7
                    0x42 => {
                        self.pty().write(b"\x1b[19~")?;
                    } // F8
                    0x43 => {
                        self.pty().write(b"\x1b[20~")?;
                    } // F9
                    0x44 => {
                        self.pty().write(b"\x1b[21~")?;
                    } // F10
                    _ => {
                        // Regular character
                        if let Some(ch) = InputEvent::scancode_to_char(scancode, self.shift_pressed)
                        {
                            self.pty().write(ch.to_string().as_bytes())?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn render(&mut self, framebuffer: &mut Framebuffer) -> Result<bool> {
        // Drain all pending PTY data into a local buffer first to avoid borrow conflict
        let mut pending: Vec<Vec<u8>> = Vec::new();
        if let Some(ref rx) = self.pty_rx {
            while let Ok(data) = rx.try_recv() {
                pending.push(data);
            }
        }

        if pending.is_empty() {
            return Ok(false);
        }

        for data in &pending {
            self.emu().process_output(data)?;
        }

        let screen = self.emu().get_screen()?;
        self.renderer.render(&screen, framebuffer)?;
        Ok(true)
    }

    fn content_notify(&self) -> Arc<Notify> {
        self.content_notify.clone()
    }
}

/// Factory for creating terminal application instances.
pub struct TerminalAppFactory {
    pub shell: Option<String>,
    pub font_size: f32,
}

impl RdpApplicationFactory for TerminalAppFactory {
    fn create(&self) -> Result<Box<dyn RdpApplication>> {
        Ok(Box::new(TerminalApp::new(
            self.shell.clone(),
            self.font_size,
        )?))
    }
}

/// Password authenticator for validating RDP client credentials.
pub struct PasswordAuthenticator {
    pub username: Option<String>,
    pub password: Option<String>,
}

impl RdpAuthenticator for PasswordAuthenticator {
    fn authenticate(&self, username: &str, password: &str) -> bool {
        if let Some(ref expected_user) = self.username {
            if username != expected_user.as_str() {
                return false;
            }
        }
        if let Some(ref expected_pass) = self.password {
            if password != expected_pass.as_str() {
                return false;
            }
        }
        true
    }
}
