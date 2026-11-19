//! GGUF grouping adapted from bodaay/HuggingFaceModelDownloader (Apache-2.0),
//! commit 20f4be12d05012afbefbf54ad13dcfc12c6560b9. Rust port uses exact catalog IDs.
use crate::{repository::EntryId, selection::Catalog};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Profile {
    pub label: String,
    pub files: Vec<EntryId>,
    pub bytes: u64,
    pub complete: bool,
}

pub fn analyze(catalog: &Catalog) -> &[Profile] {
    catalog.profiles.get_or_init(|| analyze_uncached(catalog))
}
fn analyze_uncached(catalog: &Catalog) -> Vec<Profile> {
    let mut groups: BTreeMap<String, Profile> = BTreeMap::new();
    let mut shards: BTreeMap<String, (usize, std::collections::BTreeSet<usize>)> = BTreeMap::new();
    for index in 0..catalog.len() {
        let id = EntryId {
            catalog: catalog.id,
            index: index as u32,
        };
        let Some(entry) = catalog.entry(id) else {
            continue;
        };
        let path = entry.path.as_str();
        let lower = path.to_ascii_lowercase();
        if !lower.ends_with(".gguf") {
            continue;
        }
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let base = name[..name.len() - 5].to_owned();
        let low = base.to_ascii_lowercase();
        if low.starts_with("imatrix") {
            continue;
        }
        let role = if low.starts_with("mmproj") || low.contains("-mmproj") {
            "Vision"
        } else if low.starts_with("mtp-")
            || low.starts_with("mtp_")
            || (low.contains("mtp") && low.contains("draft"))
            || dir.split('/').any(|x| x.eq_ignore_ascii_case("mtp"))
        {
            "MTP"
        } else {
            "Model"
        };
        let mut stem = base.as_str();
        let mut split = None;
        if let Some((left, total)) = stem.rsplit_once("-of-")
            && let Some((prefix, part)) = left.rsplit_once('-')
            && part.len() >= 3
            && total.len() >= 3
            && let (Ok(part), Ok(total)) = (part.parse::<usize>(), total.parse::<usize>())
        {
            stem = prefix;
            split = Some((part, total));
        }
        let key = format!("{dir}/{stem}");
        let label = stem
            .split('-')
            .enumerate()
            .find_map(|(i, token)| {
                let up = token.to_ascii_uppercase();
                let quant = up
                    .strip_prefix('Q')
                    .or_else(|| up.strip_prefix("IQ"))
                    .is_some_and(|x| x.starts_with(|c: char| c.is_ascii_digit()))
                    || matches!(up.as_str(), "F16" | "BF16" | "F32");
                (quant && up.chars().any(|c| c.is_ascii_digit())).then(|| {
                    let ud = i > 0
                        && stem
                            .split('-')
                            .nth(i - 1)
                            .is_some_and(|x| x.eq_ignore_ascii_case("UD"));
                    format!("{}{up}", if ud { "UD-" } else { "" })
                })
            })
            .unwrap_or_else(|| {
                if dir.is_empty() {
                    stem.into()
                } else {
                    dir.into()
                }
            });
        let group = groups.entry(key.clone()).or_insert_with(|| Profile {
            label: format!("{role} · {label} · {dir}/{stem}"),
            files: Vec::new(),
            bytes: 0,
            complete: true,
        });
        group.files.push(id);
        group.bytes = group.bytes.saturating_add(entry.size.get());
        if let Some((part, total)) = split {
            let expected = shards
                .entry(key)
                .or_insert_with(|| (total, Default::default()));
            if expected.0 != total || part == 0 || part > total {
                group.complete = false;
            }
            expected.1.insert(part);
        }
    }
    for (key, (total, parts)) in shards {
        if let Some(group) = groups.get_mut(&key) {
            group.complete &= total > 0 && parts.len() == total && group.files.len() == total;
        }
    }
    let mut profiles: Vec<_> = groups.into_values().collect();
    let mut labels = BTreeMap::<String, usize>::new();
    for p in &profiles {
        *labels
            .entry(
                p.label
                    .rsplit_once(" · ")
                    .map_or(p.label.as_str(), |(x, _)| x)
                    .to_owned(),
            )
            .or_default() += 1;
    }
    for p in &mut profiles {
        let short = p
            .label
            .rsplit_once(" · ")
            .map_or(p.label.as_str(), |(x, _)| x);
        if labels.get(short) == Some(&1) {
            p.label = short.to_owned();
        }
    }
    let mut documents = Vec::new();
    for index in 0..catalog.len() {
        let id = EntryId {
            catalog: catalog.id,
            index: index as u32,
        };
        if let Some(e) = catalog.entry(id) {
            let name = e.path.as_str().to_ascii_lowercase();
            if e.size.get() <= 1_048_576
                && matches!(
                    name.as_str(),
                    "config.json" | "readme.md" | "license" | "license.txt"
                )
            {
                documents.push(id);
            }
        }
    }
    // A single generic F16 projector is a shared candidate only when every
    // model belongs to the same filename family. Mixed-model repos stay explicit.
    let families: std::collections::BTreeSet<_> = profiles
        .iter()
        .filter(|p| p.label.starts_with("Model ·"))
        .filter_map(|p| {
            let e = catalog.entry(p.files[0])?;
            let name = e.path.as_str().rsplit('/').next()?;
            let mut family = Vec::new();
            for token in name.split('-') {
                let up = token.trim_end_matches(".gguf").to_ascii_uppercase();
                if up == "UD"
                    || matches!(up.as_str(), "BF16" | "F16" | "F32")
                    || up
                        .strip_prefix('Q')
                        .or_else(|| up.strip_prefix("IQ"))
                        .is_some_and(|x| x.starts_with(|c: char| c.is_ascii_digit()))
                {
                    break;
                }
                family.push(token);
            }
            Some(family.join("-"))
        })
        .collect();
    let vision = if families.len() == 1 {
        profiles.iter().flat_map(|p| &p.files).copied().find(|id| {
            catalog
                .entry(*id)
                .is_some_and(|e| e.path.as_str().eq_ignore_ascii_case("mmproj-F16.gguf"))
        })
    } else {
        None
    };
    for profile in profiles
        .iter_mut()
        .filter(|p| p.label.starts_with("Model ·"))
    {
        profile.files.extend(documents.iter().copied());
        profile.files.extend(vision);
        profile.bytes = profile
            .files
            .iter()
            .filter_map(|id| catalog.entry(*id))
            .map(|e| e.size.get())
            .fold(0u64, u64::saturating_add);
    }
    if !documents.is_empty() && !profiles.is_empty() {
        profiles.push(Profile {
            label: "Configuration / documentation".into(),
            bytes: documents
                .iter()
                .filter_map(|id| catalog.entry(*id))
                .map(|e| e.size.get())
                .fold(0u64, u64::saturating_add),
            files: documents,
            complete: true,
        });
    }
    profiles
}

/// A profile changes only its exact members; shared files required by another
/// fully selected profile remain checked when one profile is removed.
pub fn change(
    catalog: &Catalog,
    index: usize,
    selected: bool,
    current: &std::collections::BTreeSet<EntryId>,
) -> Option<crate::selection::SelectionChange> {
    let profiles = analyze(catalog);
    let profile = profiles.get(index).filter(|p| p.complete)?;
    let mut ids = profile.files.clone();
    if !selected && profile.label.starts_with("Model ·") {
        let protected: std::collections::BTreeSet<_> = profiles
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                *i != index
                    && p.label.starts_with("Model ·")
                    && p.files.iter().all(|id| current.contains(id))
            })
            .flat_map(|(_, p)| p.files.iter().copied())
            .collect();
        ids.retain(|id| !protected.contains(id));
    }
    Some(crate::selection::SelectionChange::Group { ids, selected })
}

impl Profile {
    pub fn display_label(&self, language: crate::storage::Language) -> String {
        use crate::storage::Language::*;
        let replacements = match language {
            English => return self.label.clone(),
            ZhHans => [
                ("Model ·", "主模型 ·"),
                ("Vision ·", "视觉投影 ·"),
                ("MTP ·", "MTP 加速 ·"),
                ("Configuration / documentation", "配置与说明"),
            ],
            ZhHant => [
                ("Model ·", "主模型 ·"),
                ("Vision ·", "視覺投影 ·"),
                ("MTP ·", "MTP 加速 ·"),
                ("Configuration / documentation", "設定與說明"),
            ],
        };
        for (prefix, replacement) in replacements {
            if let Some(rest) = self.label.strip_prefix(prefix) {
                return format!("{replacement}{rest}");
            }
        }
        self.label.clone()
    }
}
