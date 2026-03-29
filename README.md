# rdpterm

A secure RDP terminal server built on [rdpfb](https://github.com/dspearson/rdpfb). Connect with any RDP client (xfreerdp, mstsc, Remmina) and get a full terminal session with pixel-perfect font rendering.

## Features

- Terminal emulation via alacritty_terminal (full VTE, 256-colour, true colour)
- Font rendering via cosmic-text + swash + skrifa
  - Embedded JetBrains Mono NL Nerd Font (regular, bold, italic, bold-italic)
  - Colour emoji via Noto Color Emoji (CBDT/PNG)
  - Programmatic box-drawing, block elements, and braille characters
- Simple username/password authentication (from CLI flags or env vars)
- RDP protocol, TLS, rate limiting, and dirty-tile rendering provided by rdpfb

## Quick start

```sh
cargo build --release
./target/release/rdpterm --tls-cert certs/server.crt --tls-key certs/server.key
```

Connect:
```sh
xfreerdp /v:localhost /size:800x600 /cert:ignore /u:test /p:test
```

## CLI options

```
rdpterm [OPTIONS]

  -a, --address <ADDRESS>      Listen address [default: 0.0.0.0]
  -p, --port <PORT>            Listen port [default: 3389]
      --width <WIDTH>          Default framebuffer width [default: 1600]
      --height <HEIGHT>        Default framebuffer height [default: 900]
  -f, --font-size <FONT_SIZE>  Font size in points [default: 16]
  -s, --shell <SHELL>          Shell to spawn (defaults to $SHELL or /bin/sh)
      --tls-cert <TLS_CERT>    TLS certificate path [default: certs/server.crt]
      --tls-key <TLS_KEY>      TLS private key path [default: certs/server.key]
      --no-tls                 Disable TLS (plaintext connections)
  -u, --user <USER>            Required username [env: RDPTERM_USER]
      --password <PASSWORD>    Required password [env: RDPTERM_PASSWORD]
  -l, --log-level <LOG_LEVEL>  Log level [default: info]
```

## Authentication

By default, any credentials are accepted. To require specific credentials:

```sh
rdpterm --user admin --password secret
```

Or via environment variables:
```sh
export RDPTERM_USER=admin
export RDPTERM_PASSWORD=secret
rdpterm
```

## Generating TLS certificates

```sh
mkdir -p certs
openssl req -x509 -newkey rsa:4096 -nodes \
  -keyout certs/server.key -out certs/server.crt \
  -days 365 -subj "/CN=localhost"
```

## Architecture

```
src/
  main.rs             CLI entry point (clap)
  app.rs              TerminalApp — implements rdpfb::RdpApplication
  terminal/
    mod.rs            Module root
    emulator.rs       alacritty_terminal wrapper
    pty.rs            PTY spawn/management
    renderer.rs       Multi-path glyph rendering
fonts/                Embedded font files (SIL OFL 1.1)
```

All RDP protocol, networking, TLS, framebuffer management, and dirty-tile
rendering live in [rdpfb](https://github.com/dspearson/rdpfb). This crate
only contains the terminal-specific application logic.

## Licence

ISC. Embedded fonts are licensed under the SIL Open Font Licence 1.1.
