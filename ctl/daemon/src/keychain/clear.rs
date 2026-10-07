//! Complete attribute-only planning precedes any destructive credential action.

use std::collections::BTreeSet;

use ctl_ipc::credentials::ClearCounts;
use ctl_keychain_client::{Authentication, Record};

use super::{Error, identity, index, invalid_metadata};
use crate::credential_metadata::{self, SERVICE_PREFIX};

const MAX_ITEMS: usize = 8192;
const REASON: &str = "Remove all saved ctmux SSH passwords and identity passphrases from Keychain. Identity files, saved hosts, VPN settings, and never-save preferences are preserved.";

#[derive(Clone, Copy)]
enum Kind {
  Credential,
  Identity,
}

struct Item {
  service: String,
  account: String,
  kind: Kind,
}

trait Store {
  fn scan(&mut self) -> Result<Vec<Record>, Error>;
  fn begin_mutation(&mut self) -> Result<(), Error>;
  fn delete(&mut self, item: &Item) -> Result<(), Error>;
  fn reset_empty(&mut self) -> Result<(), Error>;
}

struct Keychain;

impl Store for Keychain {
  fn scan(&mut self) -> Result<Vec<Record>, Error> {
    ctl_keychain_client::scan_attributes(Authentication::Allow { reason: REASON }, |service| {
      service.starts_with(SERVICE_PREFIX) || service == identity::SERVICE
    })
    .map_err(Into::into)
  }

  fn begin_mutation(&mut self) -> Result<(), Error> {
    index::begin_secret_mutation().map(|_| ())
  }

  fn delete(&mut self, item: &Item) -> Result<(), Error> {
    ctl_keychain_client::delete(
      &item.service,
      Some(&item.account),
      Authentication::Allow { reason: REASON },
    )
    .map_err(Into::into)
  }

  fn reset_empty(&mut self) -> Result<(), Error> {
    index::reset_empty()
  }
}

pub(super) fn run() -> Result<ClearCounts, Error> {
  let _operation = super::operation::acquire()?;
  super::availability()?;
  run_with(&mut Keychain)
}

fn run_with(store: &mut impl Store) -> Result<ClearCounts, Error> {
  let items = plan(store.scan()?)?;
  store.begin_mutation()?;
  let mut counts = ClearCounts::default();
  for item in items {
    store.delete(&item)?;
    match item.kind {
      Kind::Credential => counts.credential_count += 1,
      Kind::Identity => counts.identity_count += 1,
    }
  }
  // This is the commit point. Any partial failure retains an incomplete index.
  store.reset_empty()?;
  Ok(counts)
}

fn plan(records: Vec<Record>) -> Result<Vec<Item>, Error> {
  if records.len() > MAX_ITEMS {
    return Err(invalid_metadata());
  }
  let mut items = Vec::new();
  let mut seen = BTreeSet::new();
  for record in records {
    // Secret data is neither requested nor accepted by this operation.
    if record.secret.is_some() {
      return Err(invalid_metadata());
    }
    let service = record.attributes.get("svce").ok_or_else(invalid_metadata)?;
    let kind = if let Some(scope) = service.strip_prefix(SERVICE_PREFIX) {
      let account = record.attributes.get("acct").ok_or_else(invalid_metadata)?;
      if credential_metadata::item_identity(&format!("{scope}:{account}")).is_none() {
        return Err(invalid_metadata());
      }
      Kind::Credential
    } else if service == identity::SERVICE {
      let account = record.attributes.get("acct").ok_or_else(invalid_metadata)?;
      if account.len() != 64
        || !account
          .bytes()
          .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
      {
        return Err(invalid_metadata());
      }
      Kind::Identity
    } else {
      continue;
    };
    let account = record.attributes.get("acct").ok_or_else(invalid_metadata)?;
    if !seen.insert((service.clone(), account.clone())) {
      // Duplicate selectors cannot establish exact deletion counts.
      return Err(invalid_metadata());
    }
    items.push(Item {
      service: service.clone(),
      account: account.clone(),
      kind,
    });
  }
  Ok(items)
}

#[cfg(test)]
mod tests;
