use crate::perms::{CommandDisposition, ensure_safe_command};
use crate::shell::ShellKind;
use anyhow::{Context, Result, anyhow};
use atty::Stream;
use serde::Deserialize;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

#[cfg(unix)]
use portable_pty::Child as PtyChild;
#[cfg(unix)]
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[derive(Debug, Deserialize, Clone)]
pub struct Args {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamChunk<'a> {
    pub kind: StreamKind,
    pub data: &'a [u8],
}

pub async fn run(
    args: Args,
    auto_yes: bool,
    debug: bool,
    shell: ShellKind,
    mut on_chunk: Option<&mut dyn for<'chunk> FnMut(StreamChunk<'chunk>)>,
) -> Result<String> {
    let cwd = args.cwd.as_deref().unwrap_or(".");
    eprintln!("Proposed command: {}", &args.command);
    eprintln!("Working directory: {}", cwd);

    let disposition = match ensure_safe_command(&args.command) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("[blocked] {}", err);
            return Err(err);
        }
    };
    let mut requires_manual_confirmation = false;
    if let CommandDisposition::NeedsConfirmation { reason } = &disposition {
        requires_manual_confirmation = true;
        eprintln!("[warn] {}", reason);
    }
    // Additional confirmation if cwd is outside the user's home directory
    if let Some(home) = dirs::home_dir() {
        use std::path::Path;
        let cwd_path = Path::new(cwd);
        // Best effort: canonicalize if possible, else use provided path
        let resolved = std::fs::canonicalize(cwd_path).unwrap_or_else(|_| cwd_path.to_path_buf());
        if !resolved.starts_with(&home) {
            eprintln!(
                "Warning: working directory is outside your home: {}",
                resolved.display()
            );
            if !auto_yes {
                if !prompt_yes_no("Proceed anyway? [y/N]: ")? {
                    return Err(anyhow!("Execution canceled by user (cwd outside home)"));
                }
            } else if debug {
                eprintln!("[debug] Proceeding due to --yes flag");
            }
        }
    }
    if (requires_manual_confirmation || !auto_yes) && !prompt_yes_no("Execute? [y/N]: ")? {
        return Err(anyhow!("Execution canceled by user"));
    }

    let child = spawn_child(&args.command, cwd, shell, debug)?;

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();

    let (tx, mut rx) = mpsc::unbounded_channel::<StreamMessage>();
    let mut killswitch = setup_stream_tasks(child, tx.clone())?;
    drop(tx);

    let collect_future = async {
        while let Some(msg) = rx.recv().await {
            match msg {
                StreamMessage::Stdout(chunk) => {
                    if let Some(handler) = on_chunk.as_deref_mut() {
                        handler(StreamChunk {
                            kind: StreamKind::Stdout,
                            data: &chunk,
                        });
                    }
                    stdout_buf.extend_from_slice(&chunk);
                }
                StreamMessage::Stderr(chunk) => {
                    if let Some(handler) = on_chunk.as_deref_mut() {
                        handler(StreamChunk {
                            kind: StreamKind::Stderr,
                            data: &chunk,
                        });
                    }
                    stderr_buf.extend_from_slice(&chunk);
                }
                StreamMessage::Error(err) => return Err(err),
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    match timeout(Duration::from_secs(120), collect_future).await {
        Ok(res) => res?,
        Err(_) => {
            killswitch.terminate().await?;
            let _ = killswitch.wait().await;
            return Err(anyhow!("Command timed out after 120s"));
        }
    }

    let code = killswitch.wait().await?;
    let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

    if debug {
        eprintln!(
            "[debug] exit code: {} (stdout {} bytes, stderr {} bytes)",
            code,
            stdout.len(),
            stderr.len()
        );
    }

    let mut summary = String::new();
    summary.push_str(&format!("Exit code: {}\n", code));
    summary.push_str("--- stdout ---\n");
    summary.push_str(&stdout);
    if !stdout.ends_with('\n') {
        summary.push('\n');
    }
    summary.push_str("--- stderr ---\n");
    summary.push_str(&stderr);
    if !stderr.ends_with('\n') {
        summary.push('\n');
    }
    Ok(summary)
}

enum StreamMessage {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Error(anyhow::Error),
}

enum ChildProcess {
    Plain(tokio::process::Child),
    #[cfg(unix)]
    Pty {
        child: Box<dyn PtyChild + Send>,
        reader: Box<dyn Read + Send>,
    },
}

struct KillSwitch {
    inner: KillSwitchInner,
}

enum KillSwitchInner {
    Plain(Option<tokio::process::Child>),
    #[cfg(unix)]
    Pty(Option<Box<dyn PtyChild + Send>>),
}

impl KillSwitch {
    async fn terminate(&mut self) -> Result<()> {
        match &mut self.inner {
            KillSwitchInner::Plain(child_opt) => {
                if let Some(child) = child_opt.as_mut() {
                    let _ = child.kill().await;
                }
            }
            #[cfg(unix)]
            KillSwitchInner::Pty(child_opt) => {
                if let Some(child) = child_opt.as_mut() {
                    let _ = child.kill();
                }
            }
        }
        Ok(())
    }

    async fn wait(self) -> Result<i32> {
        match self.inner {
            KillSwitchInner::Plain(Some(mut child)) => {
                let status = child.wait().await?;
                Ok(status.code().unwrap_or(-1))
            }
            KillSwitchInner::Plain(None) => Ok(-1),
            #[cfg(unix)]
            KillSwitchInner::Pty(Some(mut child)) => {
                let status = tokio::task::spawn_blocking(move || child.wait())
                    .await
                    .context("Joining PTY wait task failed")??;
                Ok(convert_pty_exit_status(status))
            }
            #[cfg(unix)]
            KillSwitchInner::Pty(None) => Ok(-1),
        }
    }
}

fn spawn_child(command: &str, cwd: &str, shell: ShellKind, debug: bool) -> Result<ChildProcess> {
    match shell {
        ShellKind::Posix => spawn_posix_child(command, cwd, debug),
        ShellKind::CmdExe => spawn_cmd_child(command, cwd),
        ShellKind::PowerShell => spawn_powershell_child(command, cwd),
    }
}

fn spawn_posix_child(command: &str, cwd: &str, debug: bool) -> Result<ChildProcess> {
    #[cfg(unix)]
    {
        if should_use_pty() {
            match spawn_child_with_pty(command, cwd) {
                Ok(proc) => return Ok(proc),
                Err(err) => {
                    if debug {
                        eprintln!("[debug] PTY spawn failed ({}); falling back to pipes", err);
                    }
                }
            }
        }
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command);
    configure_stdio(&mut cmd, cwd);
    let child = cmd.spawn().context("Failed to spawn command via sh")?;
    Ok(ChildProcess::Plain(child))
}

fn spawn_cmd_child(command: &str, cwd: &str) -> Result<ChildProcess> {
    #[cfg(windows)]
    let program = "cmd.exe";
    #[cfg(not(windows))]
    let program = "cmd.exe";

    let mut cmd = Command::new(program);
    cmd.arg("/d").arg("/s").arg("/c").arg(command);
    configure_stdio(&mut cmd, cwd);
    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn {}", program))?;
    Ok(ChildProcess::Plain(child))
}

fn spawn_powershell_child(command: &str, cwd: &str) -> Result<ChildProcess> {
    #[cfg(windows)]
    let candidates: &[&str] = &["pwsh.exe", "powershell.exe"];
    #[cfg(not(windows))]
    let candidates: &[&str] = &["pwsh", "powershell"]; // allow Windows shells if installed

    let mut errors = Vec::new();
    for prog in candidates {
        let mut cmd = Command::new(prog);
        cmd.arg("-NoLogo")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(command);
        configure_stdio(&mut cmd, cwd);
        match cmd.spawn() {
            Ok(child) => return Ok(ChildProcess::Plain(child)),
            Err(err) => errors.push(format!("{}: {}", prog, err)),
        }
    }

    let tried = candidates.join(", ");
    if errors.is_empty() {
        Err(anyhow!(
            "Failed to spawn PowerShell (no candidates found). Tried: {}",
            tried
        ))
    } else {
        Err(anyhow!(
            "Failed to spawn PowerShell (tried {}): {}",
            tried,
            errors.join("; ")
        ))
    }
}

fn configure_stdio(cmd: &mut Command, cwd: &str) {
    cmd.current_dir(PathBuf::from(cwd));
    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
}

fn setup_stream_tasks(
    child: ChildProcess,
    tx: mpsc::UnboundedSender<StreamMessage>,
) -> Result<KillSwitch> {
    match child {
        ChildProcess::Plain(mut child) => {
            if let Some(stdout) = child.stdout.take() {
                spawn_pipe_reader(stdout, tx.clone(), StreamKind::Stdout);
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_pipe_reader(stderr, tx.clone(), StreamKind::Stderr);
            }
            Ok(KillSwitch {
                inner: KillSwitchInner::Plain(Some(child)),
            })
        }
        #[cfg(unix)]
        ChildProcess::Pty { child, reader } => {
            spawn_pty_reader(reader, tx);
            Ok(KillSwitch {
                inner: KillSwitchInner::Pty(Some(child)),
            })
        }
    }
}

fn spawn_pipe_reader<R>(reader: R, tx: mpsc::UnboundedSender<StreamMessage>, kind: StreamKind)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = vec![0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    let msg = match kind {
                        StreamKind::Stdout => StreamMessage::Stdout(chunk),
                        StreamKind::Stderr => StreamMessage::Stderr(chunk),
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(StreamMessage::Error(anyhow!(e)));
                    break;
                }
            }
        }
    });
}

#[cfg(unix)]
fn spawn_pty_reader(reader: Box<dyn Read + Send>, tx: mpsc::UnboundedSender<StreamMessage>) {
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(StreamMessage::Stdout(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(StreamMessage::Error(anyhow!(e)));
                    break;
                }
            }
        }
    });
}

#[cfg(unix)]
fn convert_pty_exit_status(status: portable_pty::ExitStatus) -> i32 {
    status.exit_code() as i32
}

#[cfg(unix)]
fn spawn_child_with_pty(command: &str, cwd: &str) -> Result<ChildProcess> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open PTY")?;

    let mut builder = CommandBuilder::new("sh");
    builder.arg("-lc");
    builder.arg(command);
    builder.cwd(PathBuf::from(cwd));

    let child = pair
        .slave
        .spawn_command(builder)
        .context("failed to spawn child in PTY")?;
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;

    Ok(ChildProcess::Pty { child, reader })
}

#[cfg(unix)]
fn should_use_pty() -> bool {
    let force = matches!(
        std::env::var("QQQA_FORCE_PTY")
            .ok()
            .map(|v| v.to_lowercase()),
        Some(ref v) if v == "1" || v == "true" || v == "yes"
    );
    let disable = matches!(
        std::env::var("QQQA_DISABLE_PTY")
            .ok()
            .map(|v| v.to_lowercase()),
        Some(ref v) if v == "1" || v == "true" || v == "yes"
    );
    force || (!disable && atty::is(Stream::Stdout))
}

#[cfg(not(unix))]
fn should_use_pty() -> bool {
    let force = matches!(
        std::env::var("QQQA_FORCE_PTY")
            .ok()
            .map(|v| v.to_lowercase()),
        Some(ref v) if v == "1" || v == "true" || v == "yes"
    );
    force && atty::is(Stream::Stdout)
}

fn prompt_yes_no(prompt: &str) -> Result<bool> {
    eprint!("{}", prompt);
    io::stderr().flush().ok();

    if atty::is(Stream::Stdin) {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        return Ok(is_yes(&line));
    }

    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::{BufRead, BufReader};

        let file = OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .context("Failed to open /dev/tty for confirmation; re-run with --yes")?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read confirmation from /dev/tty")?;
        return Ok(is_yes(&line));
    }

    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::io::{BufRead, BufReader};

        let file = OpenOptions::new()
            .read(true)
            .open("CONIN$")
            .context("Failed to open CONIN$ for confirmation; re-run with --yes")?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read confirmation from CONIN$")?;
        return Ok(is_yes(&line));
    }

    #[allow(unreachable_code)]
    Err(anyhow!(
        "Unable to read confirmation from TTY; pass --yes to skip prompts"
    ))
}

fn is_yes(input: &str) -> bool {
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}
