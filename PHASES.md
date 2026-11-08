# Recipe lifecycle

The supported lifecycle is documented in [WRITING_RECIPES.md](WRITING_RECIPES.md#lifecycle-and-readiness).

The coordinator prepares literal dependencies in separate Ginux instances with
their own NEEDS and immutable result transfer. Inside each instance, install
checks custom installation readiness first, then build and acquisition readiness. It runs the required acquire/build/install work in dependency order,
rechecks successful work and acknowledges durable state/files pairs.

Checks return `ready(ctx)` or `needs_work(ctx, reason)`; errors fail execution.
Hooks return updated context. Only `ctx.state` is restored. Successful execution
and independent named-output validation are both required before publication.

Intermediate phases may report a declared subset of outputs; all present outputs
are validated before checkpointing. Install and installed reuse require the
complete declared set. Explicit caller ceilings apply across the dependency graph.

Manual check commands run the same checks without preparing dependencies. Remove changes the owned selected
target. Optional cleanup receives `manual` or `auto.<phase>.<success|failure>`.
See the author guide for examples and the README for recovery and diagnostics.
