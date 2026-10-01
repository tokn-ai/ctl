use std::io::{self, Write as _};

#[derive(Debug, clap::Args)]
pub struct Arguments {
  /// Skill name; short names such as host and task are also accepted.
  #[arg(value_name = "NAME", value_enum, default_value_t = Name::Ctl, conflicts_with = "list")]
  name: Name,

  /// Bundled file within the selected skill, as listed by --list.
  #[arg(
    long,
    value_name = "PATH",
    default_value = "SKILL.md",
    conflicts_with = "list"
  )]
  file: String,

  /// List bundled skill names and their available files.
  #[arg(long)]
  list: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Name {
  Ctl,
  #[value(name = "ctl-host", alias = "host")]
  Host,
  #[value(name = "ctl-session", alias = "session")]
  Session,
  #[value(name = "ctl-task", alias = "task")]
  Task,
  #[value(name = "ctl-port", alias = "port")]
  Port,
  #[value(name = "ctl-vpn", alias = "vpn")]
  Vpn,
}

impl Name {
  fn as_str(self) -> &'static str {
    match self {
      Self::Ctl => "ctl",
      Self::Host => "ctl-host",
      Self::Session => "ctl-session",
      Self::Task => "ctl-task",
      Self::Port => "ctl-port",
      Self::Vpn => "ctl-vpn",
    }
  }
}

struct Resource {
  name: Name,
  file: &'static str,
  content: &'static str,
}

// Embed the canonical skill sources so released binaries need no checkout.
const RESOURCES: &[Resource] = &[
  Resource {
    name: Name::Ctl,
    file: "SKILL.md",
    content: include_str!("../skills/ctl/SKILL.md"),
  },
  Resource {
    name: Name::Ctl,
    file: "references/setup.md",
    content: include_str!("../skills/ctl/references/setup.md"),
  },
  Resource {
    name: Name::Host,
    file: "SKILL.md",
    content: include_str!("../skills/ctl-host/SKILL.md"),
  },
  Resource {
    name: Name::Session,
    file: "SKILL.md",
    content: include_str!("../skills/ctl-session/SKILL.md"),
  },
  Resource {
    name: Name::Task,
    file: "SKILL.md",
    content: include_str!("../skills/ctl-task/SKILL.md"),
  },
  Resource {
    name: Name::Task,
    file: "references/definitions.md",
    content: include_str!("../skills/ctl-task/references/definitions.md"),
  },
  Resource {
    name: Name::Port,
    file: "SKILL.md",
    content: include_str!("../skills/ctl-port/SKILL.md"),
  },
  Resource {
    name: Name::Vpn,
    file: "SKILL.md",
    content: include_str!("../skills/ctl-vpn/SKILL.md"),
  },
  Resource {
    name: Name::Vpn,
    file: "references/setup.md",
    content: include_str!("../skills/ctl-vpn/references/setup.md"),
  },
];

pub fn run(arguments: Arguments) -> Result<(), Error> {
  let mut output = io::stdout().lock();
  if arguments.list {
    writeln!(output, "NAME\tFILE")?;
    for resource in RESOURCES {
      writeln!(output, "{}\t{}", resource.name.as_str(), resource.file)?;
    }
    return Ok(());
  }
  let resource = RESOURCES
    .iter()
    .find(|resource| resource.name == arguments.name && resource.file == arguments.file)
    .ok_or_else(|| Error::UnknownFile {
      name: arguments.name.as_str(),
      file: arguments.file,
      available: RESOURCES
        .iter()
        .filter(|resource| resource.name == arguments.name)
        .map(|resource| resource.file)
        .collect::<Vec<_>>()
        .join(", "),
    })?;
  output.write_all(resource.content.as_bytes())?;
  Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("No bundled file {file:?} exists for {name}. Available files: {available}.")]
  UnknownFile {
    name: &'static str,
    file: String,
    available: String,
  },
  #[error("Could not write skill documentation: {0}")]
  Write(#[from] io::Error),
}
