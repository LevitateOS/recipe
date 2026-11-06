use crate::llm::provider::{ProviderResult, ResolvedCall};
use crate::process::{Keep, Route, Run};
use std::time::Duration;

/// Run one provider call: prompt on stdin, bounded output, its own process
/// group, the configured deadline, and optional live tee to out-of-band stderr
/// (stdout stays reserved for the CLI's JSON).
pub(crate) fn run_call(
    mut cmd: std::process::Command,
    call: ResolvedCall,
) -> Result<ProviderResult, String> {
    if call.stdin.len() > call.max_input_bytes {
        return Err("LLM input exceeds configured byte limit".into());
    }
    cmd.current_dir(&call.cwd).envs(call.env.iter());
    let tee = |stream: bool| if stream { Route::OutOfBand } else { Route::Silent };
    let finished = Run::new(cmd)
        .stdin(call.stdin)
        .route(tee(call.stream_stdout), tee(call.stream_stderr))
        .keep(Keep::Limit(call.max_output_bytes))
        .deadline(Duration::from_secs(call.timeout_secs))
        .own_group()
        .run()
        .map_err(|error| format!("LLM subprocess failed: {error}"))?;
    Ok(ProviderResult {
        stdout: finished.stdout,
        stderr: finished.stderr,
        exit: finished.exit,
    })
}
