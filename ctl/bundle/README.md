# ctl-bundle

The default library shares ctream targets, artifact names, and agent paths
between the packer, installer, CLI, and viewer without pulling in install
dependencies.

With the `install` feature, the library verifies release ZIPs, snapshots
development builds, and selects a generation after its files are in place.
`ctlup` enables this feature for install, build, status, and launch commands.
