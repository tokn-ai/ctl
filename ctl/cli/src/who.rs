use ctl_core::presence::{ClientIdentity, ServiceConnection, connections};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Write};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub fn run(json: bool) -> io::Result<()> {
  write_connections(io::stdout().lock(), &connections()?, json)
}

fn write_connections(
  mut output: impl Write,
  records: &[ServiceConnection],
  json: bool,
) -> io::Result<()> {
  if json {
    serde_json::to_writer(&mut output, records)?;
    return writeln!(output);
  }
  if records.is_empty() {
    return writeln!(output, "No registered ctl clients.");
  }
  let rows = client_groups(records).into_iter().map(ClientGroup::row);
  writeln!(
    output,
    "{}",
    crate::table::format(
      ["DEVICE", "CLIENT", "SERVICES", "SOURCE", "CONNECTED AT"],
      rows
    )
  )
}

struct ClientGroup<'a> {
  client: Option<&'a ClientIdentity>,
  services: BTreeMap<&'a str, usize>,
  sources: BTreeSet<&'a str>,
  connected_at_ms: u64,
}

/// Keep the registry's first-seen order. An unidentified channel gets its own row.
fn client_groups(records: &[ServiceConnection]) -> Vec<ClientGroup<'_>> {
  let mut indices = HashMap::new();
  let mut groups = Vec::new();
  for record in records {
    let client = record.client.as_ref();
    let index = client.map_or(groups.len(), |identity| {
      let key = (
        &identity.instance_id,
        &identity.device_name,
        &identity.client_name,
        &identity.client_version,
      );
      *indices.entry(key).or_insert(groups.len())
    });
    if index == groups.len() {
      groups.push(ClientGroup {
        client,
        services: BTreeMap::new(),
        sources: BTreeSet::new(),
        connected_at_ms: record.connected_at_ms,
      });
    }
    let group = &mut groups[index];
    *group.services.entry(&record.service).or_default() += 1;
    if let Some(source) = record.source.as_deref() {
      group.sources.insert(source);
    }
    group.connected_at_ms = group.connected_at_ms.min(record.connected_at_ms);
  }
  groups
}

impl ClientGroup<'_> {
  fn row(self) -> [String; 5] {
    let connected =
      OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.connected_at_ms) * 1_000_000)
        .ok()
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| self.connected_at_ms.to_string());
    let services = self
      .services
      .into_iter()
      .map(|(service, count)| {
        if count == 1 {
          service.to_owned()
        } else {
          format!("{service} ({count})")
        }
      })
      .collect::<Vec<_>>()
      .join(", ");
    [
      self
        .client
        .and_then(|client| client.device_name.clone())
        .unwrap_or_else(|| "—".into()),
      self.client.map_or_else(
        || "—".into(),
        |client| format!("{} {}", client.client_name, client.client_version),
      ),
      services,
      if self.sources.is_empty() {
        "—".into()
      } else {
        self.sources.into_iter().collect::<Vec<_>>().join(", ")
      },
      connected,
    ]
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn connection(
    id: &str,
    client: Option<&ClientIdentity>,
    service: &str,
    source: &str,
    connected_at_ms: u64,
  ) -> ServiceConnection {
    ServiceConnection {
      connection_id: id.to_owned(),
      client: client.cloned(),
      service: service.to_owned(),
      source: Some(source.to_owned()),
      connected_at_ms,
    }
  }

  #[test]
  fn table_groups_channels_by_full_client_identity_but_json_keeps_every_record() {
    let client = ClientIdentity {
      instance_id: "one".into(),
      device_name: Some("Device".into()),
      client_name: "client".into(),
      client_version: "1".into(),
    };
    let second = ClientIdentity {
      instance_id: "two".into(),
      ..client.clone()
    };
    let conflicting = ClientIdentity {
      client_version: "2".into(),
      ..client.clone()
    };
    let records = [
      connection("a", Some(&client), "ctmux", "source A", 3000),
      connection("b", Some(&second), "ctmux", "source B", 2000),
      connection("c", Some(&client), "task", "source C", 1000),
      connection("d", Some(&client), "ctmux", "source A", 4000),
      connection("e", Some(&conflicting), "task", "source A", 5000),
      connection("f", None, "ctmux", "source A", 6000),
      connection("g", None, "ctmux", "source A", 6000),
    ];
    let mut output = Vec::new();
    write_connections(&mut output, &records, false).unwrap();
    let text = String::from_utf8(output).unwrap();
    let rows: Vec<_> = text.lines().skip(1).collect();
    assert_eq!(rows.len(), 5);
    assert!(rows[0].contains("ctmux (2), task"));
    assert!(rows[0].contains("source A, source C"));
    assert!(rows[0].contains("1970-01-01T00:00:01Z"));
    assert!(rows[1].contains("source B"));
    assert!(rows[2].contains("client 2"));
    assert_eq!(rows[3], rows[4]);
    let mut output = Vec::new();
    write_connections(&mut output, &records, true).unwrap();
    let raw: Vec<ServiceConnection> = serde_json::from_slice(&output).unwrap();
    assert_eq!(raw, records);
  }

  #[test]
  fn table_escapes_peer_labels_and_json_preserves_the_structured_identity() {
    let records = [ServiceConnection {
      connection_id: "test-connection".into(),
      client: Some(ClientIdentity {
        instance_id: "test-instance".into(),
        device_name: Some("Build\u{202e}device".into()),
        client_name: "fixture-client".into(),
        client_version: "0.1.2".into(),
      }),
      service: "ctmux".into(),
      source: Some("endpoint\nlabel".into()),
      connected_at_ms: 1000,
    }];
    let mut text = Vec::new();
    write_connections(&mut text, &records, false).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("Build\\u{202e}device"));
    assert!(text.contains("endpoint\\nlabel"));
    assert!(text.contains("1970-01-01T00:00:01Z"));
    let mut output = Vec::new();
    write_connections(&mut output, &records, true).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json[0]["connection_id"], "test-connection");
    assert_eq!(json[0]["connected_at_ms"], 1000);
    assert_eq!(json[0]["client"]["instance_id"], "test-instance");
    assert_eq!(json[0]["source"], "endpoint\nlabel");
  }
}
