use crate::llm::provider::{ProviderResult, ResolvedCall};
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .enable_io()
            .build()
            .expect("failed to build tokio runtime for recipe llm runner")
    })
}

async fn read_limited<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    mut tee: Option<TeeTarget>,
) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| format!("Failed to read subprocess output: {e}"))?;
        if n == 0 {
            break;
        }
        if let Some(target) = tee.as_mut() {
            // Best-effort tee; don't fail the run if terminal write fails.
            let _ = target.write_all(&chunk[..n]).await;
            let _ = target.flush().await;
        }
        if buf.len().saturating_add(n) > limit {
            return Err(format!(
                "LLM subprocess exceeded max_output_bytes (limit={limit})"
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(buf)
}

enum TeeTarget {
    Stderr(tokio::io::Stderr),
}

impl TeeTarget {
    async fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Stderr(err) => err.write_all(bytes).await,
        }
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Stderr(err) => err.flush().await,
        }
    }
}

pub(crate) fn run_call(
    cmd: tokio::process::Command,
    call: ResolvedCall,
) -> Result<ProviderResult, String> {
    let timeout = call.timeout_secs;
    runtime().block_on(async move {
        tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(timeout), run_inner(cmd,call)) =>
                result.unwrap_or_else(|_|Err(format!("LLM subprocess timed out after {timeout}s"))),
            _ = async { loop { tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if crate::cancellation::signal()!=0 { break; } } } => Err("coordinator repair cancelled".into()),
        }
    })
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
async fn run_inner(
    mut cmd: tokio::process::Command,
    call: ResolvedCall,
) -> Result<ProviderResult, String> {
    use tokio::io::AsyncWriteExt;

    cmd.current_dir(&call.cwd);
    cmd.envs(call.env.iter());
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if call.stdin.len() > call.max_input_bytes {
        return Err("LLM input exceeds configured byte limit".into());
    }
    cmd.process_group(0).kill_on_drop(true);
    let parent = unsafe { libc::getpid() };
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn LLM subprocess: {e}"))?;

    let group = ProcessGroup(child.id().ok_or("provider has no PID")?);
    let stdin_bytes = call.stdin;
    if stdin_bytes.len() > call.max_input_bytes {
        return Err(format!(
            "LLM input too large (bytes={}, max_input_bytes={})",
            stdin_bytes.len(),
            call.max_input_bytes
        ));
    }

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Failed to open subprocess stdin".to_owned())?;
    let write_fut = async move {
        stdin
            .write_all(&stdin_bytes)
            .await
            .map_err(|e| format!("Failed to write to subprocess stdin: {e}"))?;
        // Explicit close to signal EOF.
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("Failed to close subprocess stdin: {e}"))?;
        Ok::<(), String>(())
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to open subprocess stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to open subprocess stderr".to_owned())?;

    // Preserve machine-readable stdout for the recipe CLI by teeing all subprocess
    // output to stderr (shell output, LLM logs, etc).
    let stdout_tee = call
        .stream_stdout
        .then(|| TeeTarget::Stderr(tokio::io::stderr()));
    let stderr_tee = call
        .stream_stderr
        .then(|| TeeTarget::Stderr(tokio::io::stderr()));
    let stdout_task = tokio::spawn(read_limited(stdout, call.max_output_bytes, stdout_tee));
    let stderr_task = tokio::spawn(read_limited(stderr, call.max_output_bytes, stderr_tee));

    // Write stdin first. If this fails, kill and bail.
    if let Err(e) = write_fut.await {
        let _ = child.kill().await;
        return Err(e);
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed waiting for subprocess: {e}"))?;
    // A provider's exited parent cannot leave pipe-holding descendants.
    drop(group);

    let exit_code = status.code().unwrap_or(1);

    // Await output readers; if they exceeded limits, surface that.
    let stdout = stdout_task
        .await
        .map_err(|e| format!("Failed joining stdout task: {e}"))??;
    let stderr = stderr_task
        .await
        .map_err(|e| format!("Failed joining stderr task: {e}"))??;

    Ok(ProviderResult {
        stdout,
        stderr,
        exit_code,
    })
}
