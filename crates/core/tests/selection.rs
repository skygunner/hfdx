use hfdx_core::repository::*;
use hfdx_core::selection::*;

fn source() -> RepositorySpec {
    RepositorySpec {
        request: RepositoryRequest {
            endpoint: Endpoint::parse("https://huggingface.co", false).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("main").unwrap(),
        },
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    }
}
fn entry(path: &str, size: u64) -> CatalogEntry {
    CatalogEntry {
        path: RepoPath::parse(path).unwrap(),
        size: Bytes::new(size),
        content_hash: None,
    }
}
fn append(selection: &mut Selection, id: CatalogId, entries: Vec<CatalogEntry>, complete: bool) {
    selection
        .append(CatalogPage {
            catalog: id,
            entries,
            complete,
        })
        .unwrap();
}
fn paths(selection: &Selection) -> Vec<String> {
    let view = selection.view();
    let catalog = view.catalog.unwrap();
    view.visible
        .iter()
        .map(|id| catalog.entry(*id).unwrap().path.as_str().to_owned())
        .collect()
}

#[test]
fn filters_match_design_patterns_and_literal_inputs() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    let names = [
        "a/x-UD-DW-Q6_K_L.gguf",
        "b-UD-IQ4_XS.gguf",
        "c-UD-Q5_K_XL.gguf",
        "d-Q5_K_L.gguf",
        "folder/Space [1].bin",
        "folder/space x.bin",
        "folder/space 中.bin",
        "folder/a/b.txt",
        "folder/[x]{a}.txt",
        "other.gguf",
    ];
    append(
        &mut selection,
        id,
        names.iter().map(|name| entry(name, 1)).collect(),
        true,
    );
    selection
        .set_filter("*-UD-DW-Q6_K_L.gguf\n*-UD-IQ4_XS.gguf\n*-UD-Q5_K_XL.gguf\n*-Q5_K_L.gguf")
        .unwrap();
    assert_eq!(paths(&selection), names[..4]);
    for (filter, expected) in [
        (" Space [1] ", vec![names[4]]),
        ("*[1].bin", vec![names[4]]),
        ("space", vec![names[4], names[5], names[6]]),
        (
            "*.GGUF",
            vec![names[0], names[1], names[2], names[3], names[9]],
        ),
        (
            "*.GGUF\nSPACE",
            vec![
                names[0], names[1], names[2], names[3], names[4], names[5], names[6], names[9],
            ],
        ),
        ("folder/space ?.bin", vec![names[5], names[6]]),
        ("folder/**.txt", vec![names[7], names[8]]),
        ("folder/*[x]{a}.txt", vec![names[8]]),
        ("\n  \n", names.to_vec()),
        ("Space, [1]", vec![]),
    ] {
        selection.set_filter(filter).unwrap();
        assert_eq!(paths(&selection), expected, "{filter:?}");
    }
}

#[test]
fn ascii_case_folding_preserves_unicode_and_distinct_paths() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    append(
        &mut selection,
        id,
        vec![entry("Ä/Model.GGUF", 1), entry("ä/model.gguf", 2)],
        true,
    );
    selection.set_filter("ä/MODEL").unwrap();
    assert_eq!(paths(&selection), vec!["ä/model.gguf"]);
    selection.set_filter("ä/*.GGUF").unwrap();
    assert_eq!(paths(&selection), vec!["ä/model.gguf"]);
    selection.change(SelectionChange::SelectMatches).unwrap();
    selection.set_filter("Ä/*.gguf").unwrap();
    assert_eq!(paths(&selection), vec!["Ä/Model.GGUF"]);
    assert_eq!(selection.view().summary.hidden_selected_count, 1);
    assert_eq!(
        selection.freeze().unwrap().files[0].path.as_str(),
        "ä/model.gguf"
    );
}

#[test]
fn selection_persists_across_filters_pages_and_snapshots() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    append(
        &mut selection,
        id,
        vec![entry("first.bin", 10), entry("second.bin", 20)],
        false,
    );
    let first = selection.view().matches[0];
    selection
        .change(SelectionChange::Set {
            id: first,
            selected: true,
        })
        .unwrap();
    assert_eq!(
        selection.change(SelectionChange::SelectMatches),
        Err(SelectionError::NotReady)
    );
    assert!(matches!(selection.freeze(), Err(SelectionError::NotReady)));
    selection.set_filter("third").unwrap();
    selection.set_selected_only(true);
    append(&mut selection, id, vec![entry("third.bin", 30)], true);
    assert!(paths(&selection).is_empty());
    assert_eq!(selection.view().matches.len(), 1);
    selection.set_selected_only(false);
    assert_eq!(paths(&selection), vec!["third.bin"]);
    let old = selection.view();
    assert_eq!(selection.view().summary.hidden_selected_count, 1);
    selection.change(SelectionChange::SelectMatches).unwrap();
    assert_eq!(selection.view().summary.selected_bytes.get(), 40);
    assert_eq!(old.selected.len(), 1);
    selection.set_selected_only(true);
    assert_eq!(paths(&selection), vec!["third.bin"]);
    selection.set_filter("").unwrap();
    assert_eq!(paths(&selection), vec!["first.bin", "third.bin"]);
    let frozen = selection.freeze().unwrap();
    selection.change(SelectionChange::SelectMatches).unwrap();
    assert_eq!(frozen.files.len(), 2);
    assert_eq!(selection.freeze().unwrap().total_bytes.get(), 60);
    selection.change(SelectionChange::DeselectMatches).unwrap();
    assert!(matches!(
        selection.freeze(),
        Err(SelectionError::EmptySelection)
    ));
}

#[test]
fn appended_pages_preserve_filtered_order_and_hidden_selection() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    append(
        &mut selection,
        id,
        vec![entry("keep-a", 2), entry("hidden", 3)],
        false,
    );
    let initial = selection.view();
    let hidden = initial.matches[1];
    selection
        .change(SelectionChange::Set {
            id: hidden,
            selected: true,
        })
        .unwrap();
    selection.set_filter("keep").unwrap();
    let snapshot = selection.view();
    append(
        &mut selection,
        id,
        vec![entry("keep-b", 5), entry("hidden", 3), entry("other", 7)],
        false,
    );
    append(&mut selection, id, vec![entry("keep-c", 11)], true);
    assert_eq!(paths(&selection), vec!["keep-a", "keep-b", "keep-c"]);
    let view = selection.view();
    assert_eq!(view.summary.selected_count, 1);
    assert_eq!(view.summary.selected_bytes.get(), 3);
    assert_eq!(view.summary.hidden_selected_count, 1);
    assert_eq!(snapshot.matches.len(), 1);
    selection.change(SelectionChange::SelectMatches).unwrap();
    assert_eq!(selection.freeze().unwrap().total_bytes.get(), 21);
}

#[test]
fn stale_failed_duplicate_and_overflow_are_rejected() {
    let mut selection = Selection::new();
    let old = selection.begin(source()).unwrap().id;
    append(&mut selection, old, vec![entry("a", u64::MAX)], false);
    let item = selection.view().matches[0];
    selection
        .change(SelectionChange::Set {
            id: item,
            selected: true,
        })
        .unwrap();
    append(
        &mut selection,
        old,
        vec![entry("a", u64::MAX), entry("b", 1)],
        false,
    );
    let second = selection.view().matches[1];
    assert_eq!(
        selection.change(SelectionChange::Set {
            id: second,
            selected: true
        }),
        Err(SelectionError::SizeOverflow)
    );
    assert_eq!(selection.view().summary.selected_count, 1);
    assert_eq!(
        selection.append(CatalogPage {
            catalog: old,
            entries: vec![entry("a", 2)],
            complete: true
        }),
        Err(SelectionError::ConflictingEntry)
    );
    selection.fail(old);
    assert_eq!(selection.view().state, CatalogState::Failed);
    assert!(matches!(selection.freeze(), Err(SelectionError::NotReady)));
    let new = selection.begin(source()).unwrap().id;
    assert_ne!(old, new);
    assert_eq!(
        selection.append(CatalogPage {
            catalog: old,
            entries: vec![],
            complete: true
        }),
        Err(SelectionError::StaleEntry)
    );
    assert_eq!(
        selection.change(SelectionChange::Set {
            id: item,
            selected: true
        }),
        Err(SelectionError::StaleEntry)
    );
}

#[test]
fn profiles_keep_exact_variants_shards_and_roles_and_bulk_preview() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    append(
        &mut selection,
        id,
        vec![
            entry("Qwen3.8-27B-UD-Q4_K_M.gguf", 100),
            entry("Qwen3.8-27B-UD-Q4_K_XL.gguf", 200),
            entry("BF16/Model-BF16-00001-of-00002.gguf", 300),
            entry("BF16/Model-BF16-00002-of-00002.gguf", 300),
            entry("broken/Model-Q8_0-00001-of-00002.gguf", 400),
            entry("MTP/mtp-Qwen3.8-Q4_0.gguf", 50),
            entry("mmproj-F16.gguf", 20),
            entry("imatrix_unsloth.gguf", 90),
            entry("config.json", 10),
        ],
        true,
    );
    let catalog = selection.view().catalog.unwrap();
    let profiles = hfdx_core::profiles::analyze(&catalog);
    assert_eq!(profiles.len(), 7);
    assert!(!profiles.iter().any(|p| p.label.contains("imatrix")));
    let q4 = profiles
        .iter()
        .position(|p| p.label.starts_with("Model · UD-Q4_K_M"))
        .unwrap();
    let xl = profiles
        .iter()
        .position(|p| p.label.starts_with("Model · UD-Q4_K_XL"))
        .unwrap();
    let bf = profiles
        .iter()
        .find(|p| p.label.starts_with("Model · BF16"))
        .unwrap();
    assert_eq!(bf.files.len(), 3);
    assert_eq!(bf.bytes, 610);
    assert!(
        !profiles
            .iter()
            .find(|p| p.label.starts_with("Model · Q8_0"))
            .unwrap()
            .complete
    );
    assert!(profiles.iter().any(|p| p.label.starts_with("MTP · Q4_0")));
    for index in [q4, xl] {
        selection
            .change(
                hfdx_core::profiles::change(&catalog, index, true, &selection.view().selected)
                    .unwrap(),
            )
            .unwrap();
    }
    assert_eq!(selection.view().summary.selected_count, 3);
    assert_eq!(selection.view().summary.selected_bytes.get(), 310);
    selection.set_filter("nothing").unwrap();
    assert_eq!(selection.view().summary.hidden_selected_count, 3);
    selection
        .change(
            hfdx_core::profiles::change(&catalog, q4, false, &selection.view().selected).unwrap(),
        )
        .unwrap();
    assert_eq!(selection.view().summary.selected_count, 2);
    assert_eq!(selection.view().summary.selected_bytes.get(), 210);
}

#[test]
fn profiles_share_projector_across_quantized_and_bf16_variants() {
    let mut selection = Selection::new();
    let id = selection.begin(source()).unwrap().id;
    append(
        &mut selection,
        id,
        vec![
            entry("Qwen3.8-27B-Q4_0.gguf", 10),
            entry("Qwen3.8-27B-UD-Q3_K_XL.gguf", 20),
            entry("BF16/Qwen3.8-27B-BF16-00001-of-00002.gguf", 30),
            entry("BF16/Qwen3.8-27B-BF16-00002-of-00002.gguf", 30),
            entry("mmproj-F16.gguf", 5),
            entry("mmproj-BF16.gguf", 6),
            entry("MTP/mtp-Qwen3.8-27B-Q4_0.gguf", 7),
            entry("config.json", 1),
        ],
        true,
    );
    let catalog = selection.view().catalog.unwrap();
    let profiles = hfdx_core::profiles::analyze(&catalog);
    let projector = profiles
        .iter()
        .find(|p| p.label.starts_with("Vision · F16"))
        .unwrap()
        .files[0];
    let models: Vec<_> = profiles
        .iter()
        .enumerate()
        .filter(|(_, p)| p.label.starts_with("Model ·"))
        .collect();
    assert_eq!(models.len(), 3);
    assert!(models.iter().all(|(_, p)| p.files.contains(&projector)));
    for (index, _) in models.iter().take(2) {
        selection
            .change(
                hfdx_core::profiles::change(&catalog, *index, true, &selection.view().selected)
                    .unwrap(),
            )
            .unwrap();
    }
    let before = selection.view().summary.selected_bytes.get();
    let first = models[0];
    selection
        .change(
            hfdx_core::profiles::change(&catalog, first.0, false, &selection.view().selected)
                .unwrap(),
        )
        .unwrap();
    assert!(selection.view().selected.contains(&projector));
    assert!(selection.view().summary.selected_bytes.get() < before);
    let vision = profiles
        .iter()
        .position(|p| p.label.starts_with("Vision · F16"))
        .unwrap();
    selection
        .change(
            hfdx_core::profiles::change(&catalog, vision, false, &selection.view().selected)
                .unwrap(),
        )
        .unwrap();
    assert!(
        !selection.view().selected.contains(&projector),
        "explicit text-only custom choice remains possible"
    );
}
