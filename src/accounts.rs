//! Accounts a system change needs in the root it is written into.
//!
//! The lines are `sysusers.d` lines: those of the change's own
//! `usr/lib/sysusers.d/*.conf` files (Fedora packages ship them), then those a
//! recipe declares (`target_finalize(root, #{ owners: "apply", accounts: [...] })`,
//! for Alpine packages, whose install scripts never run). They are made the way
//! the OS image builder makes them (`fedora-rpm-package-effects.rhai`, `sysusers`),
//! which is how systemd-sysusers makes them: `g` lines first, then `u` lines; an
//! account already there is kept; an id of `-` is the highest free system id
//! below 1000. Other line kinds are ignored, as the builder ignores them.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One account a change created.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Account {
    /// `user` or `group`.
    pub kind: String,
    pub name: String,
    pub id: u32,
}

/// The four account files of a root, as text.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Files {
    pub passwd: String,
    pub group: String,
    pub shadow: String,
    pub gshadow: String,
}

/// The words of a sysusers.d line; '…' and "…" quote one word.
pub fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut open = false;
    for c in line.chars() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            open = true;
        } else if c == ' ' || c == '\t' {
            if open || !word.is_empty() {
                out.push(std::mem::take(&mut word));
                open = false;
            }
        } else {
            word.push(c);
            open = true;
        }
    }
    if open || !word.is_empty() {
        out.push(word);
    }
    out
}

/// Check a declared account line: a `u` or `g` line with a name.
pub fn check_declared(line: &str) -> Result<()> {
    let w = words(line.trim());
    ensure!(
        w.len() >= 2 && (w[0] == "u" || w[0] == "g"),
        "account declaration {line:?} is not a sysusers.d `u` or `g` line \
         (for example \"g polkitd -\" or \"u polkitd -:polkitd 'polkit' /var/lib/polkit /sbin/nologin\")"
    );
    valid_name(&w[1])?;
    Ok(())
}

fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !name.starts_with('-'),
        "invalid account name {name:?}"
    );
    Ok(())
}

/// The ids in a passwd or group file, by name.
pub fn ids(text: &str) -> BTreeMap<String, u32> {
    let mut map = BTreeMap::new();
    for line in text.split('\n') {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 3
            && let Ok(id) = fields[2].parse()
        {
            map.insert(fields[0].to_owned(), id);
        }
    }
    map
}

fn parse_id(text: &str, line: &str) -> Result<u32> {
    text.parse()
        .with_context(|| format!("sysusers.d line {line:?}: {text:?} is not a numeric id"))
}

fn free(used: &BTreeMap<String, u32>) -> Result<u32> {
    let mut id: u32 = 999;
    while used.values().any(|v| *v == id) {
        id = id
            .checked_sub(1)
            .context("no free system id is left below 1000")?;
    }
    Ok(id)
}

fn shadow_line(name: &str, password: &str) -> String {
    format!("{name}:{password}:20000:0:99999:7:::\n")
}

/// Make the accounts `lines` (each with the index of the package it came
/// from) name in `files`. Returns the new files and, per package index, the
/// accounts it created.
pub fn create(
    mut files: Files,
    lines: &[(usize, String)],
) -> Result<(Files, BTreeMap<usize, Vec<Account>>)> {
    for text in [
        &mut files.passwd,
        &mut files.group,
        &mut files.shadow,
        &mut files.gshadow,
    ] {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
    }
    let mut users = ids(&files.passwd);
    let mut groups = ids(&files.group);
    let mut created: BTreeMap<usize, Vec<Account>> = BTreeMap::new();
    for kind in ["g", "u"] {
        for (owner, line) in lines {
            let w = words(line.trim());
            if w.len() < 2 || w[0] != kind {
                continue;
            }
            let name = &w[1];
            valid_name(name).with_context(|| format!("sysusers.d line {line:?}"))?;
            let id = w.get(2).map_or("-", String::as_str);
            if kind == "g" {
                if groups.contains_key(name) {
                    continue;
                }
                let gid = if id == "-" {
                    free(&groups)?
                } else {
                    parse_id(id, line)?
                };
                groups.insert(name.clone(), gid);
                files.group += &format!("{name}:x:{gid}:\n");
                files.gshadow += &format!("{name}:!::\n");
                created.entry(*owner).or_default().push(Account {
                    kind: "group".into(),
                    name: name.clone(),
                    id: gid,
                });
                continue;
            }
            if users.contains_key(name) {
                continue;
            }
            let (uid, gid);
            if let Some((u, g)) = id.split_once(':') {
                uid = parse_id(u, line)?;
                gid = match groups.get(g) {
                    Some(gid) => *gid,
                    None => parse_id(g, line)?,
                };
            } else {
                uid = if id == "-" {
                    // Every user id and every group id is taken. (The builder's
                    // `users + groups` lets a group's id hide a same-named
                    // user's, which can hand out a user id twice; ids differ
                    // from the builder's only where it would do that.)
                    let mut used: BTreeMap<String, u32> =
                        users.iter().map(|(k, v)| (format!("u:{k}"), *v)).collect();
                    used.extend(groups.iter().map(|(k, v)| (format!("g:{k}"), *v)));
                    free(&used)?
                } else {
                    parse_id(id, line)?
                };
                if let Some(g) = groups.get(name) {
                    gid = *g;
                } else {
                    gid = if groups.values().any(|v| *v == uid) {
                        free(&groups)?
                    } else {
                        uid
                    };
                    groups.insert(name.clone(), gid);
                    files.group += &format!("{name}:x:{gid}:\n");
                    files.gshadow += &format!("{name}:!::\n");
                    created.entry(*owner).or_default().push(Account {
                        kind: "group".into(),
                        name: name.clone(),
                        id: gid,
                    });
                }
            }
            let field = |i: usize, default: &str| -> String {
                match w.get(i) {
                    Some(v) if v != "-" => v.clone(),
                    _ => default.to_owned(),
                }
            };
            let gecos = field(3, "");
            let home = field(4, "/");
            let shell = field(5, "/usr/sbin/nologin");
            for value in [&gecos, &home, &shell] {
                if value.contains([':', '\n']) {
                    bail!("sysusers.d line {line:?} has a field with ':' or a newline");
                }
            }
            users.insert(name.clone(), uid);
            files.passwd += &format!("{name}:x:{uid}:{gid}:{gecos}:{home}:{shell}\n");
            files.shadow += &shadow_line(name, "!*");
            created.entry(*owner).or_default().push(Account {
                kind: "user".into(),
                name: name.clone(),
                id: uid,
            });
        }
    }
    Ok((files, created))
}
