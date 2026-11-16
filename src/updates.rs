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

/// The newest release of a GitHub repository, as `UPSTREAM.github` asks
/// (docs/recipe-format.md, section 2.9). Contacts GitHub: call it only when
/// the person asked for the upstream check.
pub fn github_newest(repo: &str, prefix: Option<&str>) -> Result<String, String> {
    crate::helpers::acquire::http::github_newest_release(repo, prefix)
}

/// Whether version text `latest` is newer than `current`: compared in runs of
/// digits (as numbers) and runs of other characters, left to right; a version
/// that continues a shorter equal one is newer. Equal texts are not newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn chunks(version: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut digits = None;
        for (at, character) in version.char_indices() {
            if !character.is_ascii_alphanumeric() {
                if let Some(is_digits) = digits.take() {
                    out.push((is_digits, &version[start..at]));
                }
                continue;
            }
            let is_digits = character.is_ascii_digit();
            match digits {
                Some(previous) if previous == is_digits => {}
                Some(previous) => {
                    out.push((previous, &version[start..at]));
                    start = at;
                    digits = Some(is_digits);
                }
                None => {
                    start = at;
                    digits = Some(is_digits);
                }
            }
        }
        if let Some(is_digits) = digits {
            out.push((is_digits, &version[start..]));
        }
        out
    }
    let (latest, current) = (chunks(latest), chunks(current));
    for (new, old) in latest.iter().zip(&current) {
        let order = match (new.0, old.0) {
            (true, true) => {
                let (new, old) = (new.1.trim_start_matches('0'), old.1.trim_start_matches('0'));
                new.len().cmp(&old.len()).then_with(|| new.cmp(old))
            }
            // A number is newer than a word at the same place ("1.2.0" over "1.2.rc").
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => new.1.cmp(old.1),
        };
        if order != std::cmp::Ordering::Equal {
            return order == std::cmp::Ordering::Greater;
        }
    }
    latest.len() > current.len()
}
