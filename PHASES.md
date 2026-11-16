# Recipe lifecycle

Recipe has four steps: download, build, install and clean-up. `install` runs
every step still missing, in order, and then clean-up; each step can also be run
alone. Recipe decides what is done from its own records (a download by its
SHA-256, a result by its identity and recorded checksums, an installed program
by its receipt), never by running recipe code. `recipe state NAME` prints the
result: `none`, `downloaded`, `built` or `installed`.

After a successful install Recipe deletes the downloads, the unpacked source,
the build material and the saved state, unless you chose `--keep`. Failed or
interrupted work never replaces a valid earlier result, and nothing is
published unless the run succeeded and the output validated.

Where to read more:

- [Steps, states and saved values](WRITING_RECIPES.md#steps-states-and-saved-values)
  in the author guide.
- [Steps, states and commands](docs/recipe-format.md#6-steps-states-and-commands)
  in the specification, with the disk rules and the keep option.
- The [README](README.md#state-diagnostics-and-recovery) for recovery and
  diagnostics.
