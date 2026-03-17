# Beebo

Real-time SSH fleet monitoring TUI — CPU, Memory, and Network sparklines across all your hosts, right in your terminal.

## Features

- **Dashboard Grid** — All SSH hosts from `~/.ssh/config` displayed as tiles, auto-connecting on launch
- **Live Sparklines** — CPU / Memory / Network usage charts updated every 2 seconds
- **Focus Mode** — Enter any tile to get a full interactive SSH terminal (Esc to return)
- **Smart Auth** — Key-based auth by default, falls back to password input in the TUI
- **Auto Reconnect** — Focus a disconnected host to trigger reconnection

## Keybindings

| Key | Action |
|-----|--------|
| `↑↓←→` / `hjkl` | Navigate tiles |
| `Enter` | Focus (expand) selected tile |
| `Esc` | Back to grid / Quit |
| `Tab` | Cycle metric: CPU → MEM → NET |
| `1` `2` `3` | Select CPU / MEM / NET directly |
| `q` | Quit |

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Make sure you have hosts in ~/.ssh/config
beebo
```

## Requirements

- Rust 1.85+
- Remote hosts must be Linux (metrics read from `/proc`)

## License

[MIT](LICENSE)
