# Repository guidance

## Worktree development

- Use the assigned Git worktree for the entire task.
- Keep build output and test profiles in named directories under the assigned
  worktree's ignored `target/` directory.
- The task or launcher that starts a process owns its cleanup. Track the
  processes and paths it creates. Stop those processes on success, failure, or
  interruption, verify they exited, then remove their temporary data. A process
  stays running after the task only when the user requests a handoff.

## Pull requests

- Keep titles concise and use Conventional Commits format.
- When a protocol changes, include a brief protocol/version note in the title,
  for example `feat: add ctl passwords (helper 1.1.3)`. Put the details in the
  description.
- Every PR description must state its protocol impact. For each changed named
  contract, give its previous and new versions and internal builds, describe the
  operation, field, or negotiation change, and state retained compatibility or
  the explicitly announced breaking change.
- When no protocol changes, write `Protocol changes: none.` explicitly.
- Distinguish product release versions from each named protocol's version and
  build. Follow [protocol versioning](docs/protocol-versioning.md).
