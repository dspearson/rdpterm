/// Terminal module
///
/// Integrates terminal emulation (alacritty_terminal), font rendering
/// (cosmic-text/swash/skrifa), and PTY management (portable-pty).
pub mod emulator;
pub mod pty;
pub mod renderer;

pub use emulator::TerminalEmulator;
pub use pty::{PtyConfig, PtySession};
pub use renderer::{RendererConfig, TerminalRenderer};
