# ctl-paths

Shared per-user storage root for ctl and ctmux. `directory()` resolves
`~/.tokn/ctl` on all platforms without creating files or importing old data.
Configuration and persistent component state use this root; runtime sockets
continue to use their platform-specific private endpoints.
