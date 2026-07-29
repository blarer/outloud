//! `shell-bridge` CLI: daemon, control verbs, and the installer.
//!
//! Subcommands are deliberately tiny wrappers over the library so tests
//! exercise the same code paths:
//!
//! ```text
//! shell-bridge serve [--socket PATH] [--max-connections N]
//! shell-bridge intent "change prod to staging" [--socket PATH]
//! shell-bridge status | peek                    [--socket PATH]
//! shell-bridge install [--shell bash|zsh|fish] [--rc PATH] [--plugin-dir DIR]
//! shell-bridge print-plugin-path [--shell ...]
//! ```

// The whole CLI is unix-only for the same reason the library's server and
// peer modules are: the transport is a unix socket and the clients are
// POSIX shell line editors. On other targets the binary still builds (so
// `cargo build --workspace` stays one command everywhere) but says so.
#[cfg(not(unix))]
fn main() {
    eprintln!(
        "shell-bridge: unsupported on this platform (needs unix-domain sockets \
         and a POSIX shell; see docs/shell-integration.md)"
    );
    std::process::exit(1);
}

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use base64::engine::general_purpose::STANDARD as B64;
#[cfg(unix)]
use base64::Engine;

#[cfg(unix)]
use shell_bridge::protocol::Response;
#[cfg(unix)]
use shell_bridge::server::{default_socket_path, request, Server};

#[cfg(unix)]
fn main() {
    if let Err(e) = run() {
        eprintln!("shell-bridge: {e}");
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verb = args.first().map(String::as_str).unwrap_or("help");

    // Flag parsing by hand: a handful of `--flag value` pairs do not justify
    // a clap dependency in a crate whose whole point is minimal deployability.
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // First non-flag word after the verb, skipping flag values.
    let positional = || -> Option<&String> {
        let mut skip_next = false;
        args.iter().skip(1).find(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if a.starts_with("--") {
                skip_next = true;
                return false;
            }
            true
        })
    };
    let socket = flag("--socket")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);

    match verb {
        "serve" => {
            let max = flag("--max-connections")
                .map(|s| s.parse::<u64>())
                .transpose()
                .map_err(|_| anyhow::anyhow!("--max-connections must be a number"))?;
            let mut server = Server::bind(&socket)?;
            eprintln!("shell-bridge: listening on {}", socket.display());
            server.serve(max)
        }
        "intent" => {
            let utterance = positional()
                .ok_or_else(|| anyhow::anyhow!("usage: shell-bridge intent \"<utterance>\""))?;
            let resp = request(&socket, &format!("INTENT {}", B64.encode(utterance)))?;
            print_response(&resp)
        }
        "status" => print_response(&request(&socket, "STATUS")?),
        "peek" => print_response(&request(&socket, "PEEK")?),
        "print-plugin-path" => {
            let shell = flag("--shell").unwrap_or_else(detect_shell);
            let dir = plugin_dir(flag("--plugin-dir"))?;
            println!("{}", plugin_path(&dir, &shell)?.display());
            Ok(())
        }
        "install" => {
            let shell = flag("--shell").unwrap_or_else(detect_shell);
            let dir = plugin_dir(flag("--plugin-dir"))?;
            install(&shell, flag("--rc").map(PathBuf::from), &dir)
        }
        _ => {
            eprintln!(
                "usage: shell-bridge <serve|intent|status|peek|install|print-plugin-path> [flags]"
            );
            Ok(())
        }
    }
}

#[cfg(unix)]
fn print_response(resp: &Response) -> anyhow::Result<()> {
    match resp {
        Response::Ok => println!("ok"),
        Response::Status { text } => println!("{text}"),
        Response::Buffer { buffer } => println!("{buffer}"),
        Response::Noop { reason } => println!("noop: {reason}"),
        Response::Err { reason } => anyhow::bail!("{reason}"),
        Response::Replace { buffer, .. } => println!("replace: {buffer}"),
    }
    Ok(())
}

/// The user's login shell, from `$SHELL`. Asking the kernel about the parent
/// process would identify the *invoking* shell instead, which is wrong when
/// the user runs the installer from a script.
#[cfg(unix)]
fn detect_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(str::to_string))
        .unwrap_or_else(|| "zsh".into())
}

/// Where the plugin files live. During development they sit in `shell/` at
/// the workspace root, which is two levels above `target/{debug,release}`;
/// an installed build would ship them alongside the binary. The walk keeps
/// both working without configuration.
#[cfg(unix)]
fn plugin_dir(explicit: Option<String>) -> anyhow::Result<PathBuf> {
    if let Some(d) = explicit {
        return Ok(PathBuf::from(d));
    }
    let exe = std::env::current_exe()?;
    let mut dir = exe.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        let candidate = d.join("shell");
        if candidate.join("outloud.zsh").exists() {
            return Ok(candidate);
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    // Last resort: relative to the current directory, for `cargo run` from
    // the workspace root.
    Ok(PathBuf::from("shell"))
}

#[cfg(unix)]
fn plugin_path(dir: &Path, shell: &str) -> anyhow::Result<PathBuf> {
    let file = match shell {
        "bash" => "outloud.bash",
        "zsh" => "outloud.zsh",
        "fish" => "outloud.fish",
        other => anyhow::bail!("unsupported shell '{other}' (bash, zsh, fish)"),
    };
    Ok(dir.join(file))
}

/// Append one guarded `source` line to the shell's rc file. One line is the
/// whole contract: frameworks (oh-my-zsh, prezto, bash-it) all end up
/// sourcing the same rc, so composing with them means not fighting over it.
#[cfg(unix)]
fn install(shell: &str, rc_override: Option<PathBuf>, dir: &Path) -> anyhow::Result<()> {
    let plugin = plugin_path(dir, shell)?;
    if !plugin.exists() {
        anyhow::bail!("plugin file not found: {}", plugin.display());
    }
    let plugin = plugin.canonicalize()?;

    let home = std::env::var("HOME").map(PathBuf::from)?;
    let rc = match rc_override {
        Some(rc) => rc,
        None => match shell {
            // ZDOTDIR is how zsh users (and our own tests) relocate config.
            "zsh" => std::env::var("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.clone())
                .join(".zshrc"),
            "bash" => home.join(".bashrc"),
            // fish sources conf.d/*.fish automatically: drop-in, no rc edit.
            "fish" => home.join(".config/fish/conf.d/outloud.fish"),
            other => anyhow::bail!("unsupported shell '{other}'"),
        },
    };

    if shell == "fish" {
        // For fish, "installation" is a symlink into conf.d; conf.d files
        // load independently in alphabetical order, so no guard line needed.
        if let Some(parent) = rc.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if rc.symlink_metadata().is_ok() {
            std::fs::remove_file(&rc)?;
        }
        // A conf.d symlink from a pre-rename install points at a plugin
        // file that no longer exists; fish silently skips a dangling
        // symlink, so the user's binding would just vanish. Remove it.
        let legacy = rc.with_file_name("aqua.fish");
        if legacy.symlink_metadata().is_ok() {
            std::fs::remove_file(&legacy)?;
            println!("removed pre-rename plugin link: {}", legacy.display());
        }
        std::os::unix::fs::symlink(&plugin, &rc)?;
        println!("installed: {} -> {}", rc.display(), plugin.display());
        return Ok(());
    }

    let marker = "# outloud shell-bridge";
    // Pre-rename installs left "# aqua shell-bridge" plus a source line for
    // a shell/aqua.* file that no longer exists. Rewriting our own old
    // marker block (marker line + the guarded source line under it) is what
    // keeps `install` idempotent across the rename instead of silently
    // leaving a dead source line and appending a second block.
    let legacy_marker = "# aqua shell-bridge";
    let line = format!(
        "{marker}\n[ -f \"{p}\" ] && source \"{p}\"\n",
        p = plugin.display()
    );
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    if existing.contains(marker) {
        println!("already installed in {}", rc.display());
        return Ok(());
    }
    if existing.contains(legacy_marker) {
        let rewritten: String = existing
            .lines()
            .scan(false, |skip_next, l| {
                if *skip_next {
                    *skip_next = false;
                    // The guarded source line that followed the old marker.
                    if l.contains("&& source ") {
                        return Some(None);
                    }
                }
                if l.trim() == legacy_marker {
                    *skip_next = true;
                    return Some(None);
                }
                Some(Some(l))
            })
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        let updated = format!("{}\n\n{line}", rewritten.trim_end());
        std::fs::write(&rc, updated)?;
        println!("updated pre-rename install in {}", rc.display());
        println!("restart your shell or: source {}", rc.display());
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc)?;
    // Leading newline so we never glue onto a file that lacks a trailing one.
    write!(f, "\n{line}")?;
    println!("installed into {}", rc.display());
    println!("restart your shell or: source {}", rc.display());
    Ok(())
}
