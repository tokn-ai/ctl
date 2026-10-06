# Repository guidance

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
