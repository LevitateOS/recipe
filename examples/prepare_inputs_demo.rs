//! Show immutable input discovery and domain configuration without evaluating
//! recipe code. Pass a recipe path followed by optional NAME=PATH inputs.
use levitate_recipe::inputs::{ExtraInput, prepare_invocation};
use std::path::PathBuf;
fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let recipe = arguments.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../distro-builder/recipes/recfstab.rhai")
    });
    let mut inputs = Vec::new();
    for argument in arguments {
        let text = argument
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("input must be Unicode NAME=PATH"))?;
        let (name, path) = text
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("input requires NAME=PATH"))?;
        inputs.push(ExtraInput {
            name: name.to_owned(),
            path: PathBuf::from(path),
        });
    }
    let invocation =
        prepare_invocation(&recipe, "install", None, &inputs, &[], &Default::default())?;
    println!("{}\n{}", invocation.invocation_id, invocation.config_text);
    for input in &invocation.inputs {
        println!(
            "/inputs/{} sha256={} size={} mode={:04o} kind={:?}",
            input.name,
            hex::encode(input.sha256),
            input.size,
            input.mode,
            input.kind
        );
    }
    Ok(())
}
