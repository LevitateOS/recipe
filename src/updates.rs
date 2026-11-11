//! What an update would change in what a recipe may use: the NEEDS recorded
//! when a package was installed, set beside the NEEDS of the recipe that
//! would replace it. Nothing is run and nothing is fetched.

use crate::needs::{Needs, PathKind};
use serde::Serialize;
use std::collections::BTreeMap;

/// One field of NEEDS that differs. `kind` says in which direction: `added`
/// and `raised` mean the newer recipe may use more than the installed one.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Change {
    pub field: &'static str,
    pub before: String,
    pub after: String,
    /// `added`, `removed`, `raised`, `lowered` or `changed`.
    pub kind: &'static str,
}

fn listed(names: &[String]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

/// A list of names: `added` as soon as the newer one holds a name the older
/// one lacks.
fn names(field: &'static str, before: &[String], after: &[String]) -> Option<Change> {
    (before != after).then(|| Change {
        field,
        before: listed(before),
        after: listed(after),
        kind: if after.iter().any(|name| !before.contains(name)) {
            "added"
        } else if before.iter().any(|name| !after.contains(name)) {
            "removed"
        } else {
            "changed"
        },
    })
}

fn amount(field: &'static str, before: u64, after: u64, unit: &str) -> Option<Change> {
    (before != after).then(|| Change {
        field,
        before: format!("{before} {unit}"),
        after: format!("{after} {unit}"),
        kind: if after > before { "raised" } else { "lowered" },
    })
}

fn kinds<T>(map: &BTreeMap<String, T>, word: impl Fn(&T) -> String) -> Vec<String> {
    map.iter()
        .map(|(name, value)| format!("{name} ({})", word(value)))
        .collect()
}

/// Every field of NEEDS in which `after` differs from `before`, in the order
/// a person is told: what it may reach first, then what it may take.
pub fn changes(before: &Needs, after: &Needs) -> Vec<Change> {
    let sorted = |list: &[String]| {
        let mut list = list.to_vec();
        list.sort();
        list
    };
    let input = |need: &crate::needs::InputNeed| {
        if need.optional {
            format!("optional {}", need.kind.as_str())
        } else {
            need.kind.as_str().to_owned()
        }
    };
    let output = |kind: &PathKind| kind.as_str().to_owned();
    let storage_bytes = |bytes: Option<u64>| match bytes {
        Some(bytes) => format!("{bytes} bytes"),
        None => "bounded by memory".to_owned(),
    };
    [
        names("network", &sorted(&before.network), &sorted(&after.network)),
        amount(
            "network_max_bytes",
            before.network_max_bytes,
            after.network_max_bytes,
            "bytes",
        ),
        names(
            "inputs",
            &kinds(&before.inputs, input),
            &kinds(&after.inputs, input),
        ),
        names(
            "outputs",
            &kinds(&before.outputs, output),
            &kinds(&after.outputs, output),
        ),
        amount("memory_mib", before.memory_mib, after.memory_mib, "MiB"),
        amount(
            "storage_max_objects",
            before.storage_max_objects,
            after.storage_max_objects,
            "objects",
        ),
        (before.storage_max_bytes != after.storage_max_bytes).then(|| Change {
            field: "storage_max_bytes",
            before: storage_bytes(before.storage_max_bytes),
            after: storage_bytes(after.storage_max_bytes),
            kind: match (before.storage_max_bytes, after.storage_max_bytes) {
                (Some(before), Some(after)) if after > before => "raised",
                (Some(_), Some(_)) => "lowered",
                _ => "changed",
            },
        }),
        amount("timeout_secs", before.timeout_secs, after.timeout_secs, "s"),
    ]
    .into_iter()
    .flatten()
    .collect()
}
