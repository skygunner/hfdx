use hfdx_core::{
    Bytes, CatalogEntry, CatalogPage, CommitId, Endpoint, RepoId, RepoKind, RepoPath,
    RepositoryRequest, RepositorySpec, Revision, Selection, SelectionChange,
};
use std::{
    error::Error,
    hint::black_box,
    io::{self, Write},
    time::Instant,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    writeln!(
        output,
        "size,operation,iteration,elapsed_us,visible,selected"
    )?;
    for size in [10_000usize, 100_000] {
        let source = RepositorySpec {
            request: RepositoryRequest {
                endpoint: Endpoint::parse("https://huggingface.co", false)?,
                kind: RepoKind::Model,
                repo_id: RepoId::parse("fixture/measure")?,
                requested_revision: Revision::parse("main")?,
            },
            resolved_commit: CommitId::parse(&"a".repeat(40))?,
        };
        let mut selection = Selection::new();
        let catalog = selection.begin(source)?.id;
        let suffixes = [
            "UD-DW-Q6_K_L.gguf",
            "UD-IQ4_XS.gguf",
            "UD-Q5_K_XL.gguf",
            "Q5_K_L.gguf",
            "F16.gguf",
            "config.json",
            "README.md",
            "tokenizer.json",
        ];
        for start in (0..size).step_by(1000) {
            let end = (start + 1000).min(size);
            let entries = (start..end)
                .map(|index| {
                    Ok(CatalogEntry {
                        path: RepoPath::parse(&format!(
                            "folder{:03}/model-{index:07}-{}",
                            index % 100,
                            suffixes[index % 8]
                        ))?,
                        size: Bytes::new(1024),
                        content_hash: None,
                    })
                })
                .collect::<Result<Vec<_>, hfdx_core::RepositoryError>>()?;
            let began = Instant::now();
            selection.append(CatalogPage {
                catalog,
                entries,
                complete: end == size,
            })?;
            let view = black_box(selection.view());
            assert_eq!(view.visible.len(), end);
            writeln!(
                output,
                "{size},append,{start},{},{},0",
                began.elapsed().as_micros(),
                view.visible.len()
            )?;
        }
        let filters = [
            "",
            "model-000",
            "*-UD-DW-Q6_K_L.gguf\n*-UD-IQ4_XS.gguf\n*-UD-Q5_K_XL.gguf\n*-Q5_K_L.gguf",
            "folder???/model-???????-F16.gguf",
            "README.md",
            "not-present",
        ];
        for iteration in 0..60 {
            let filter = filters[iteration % filters.len()];
            let began = Instant::now();
            selection.set_filter(black_box(filter))?;
            let view = black_box(selection.view());
            let elapsed = began.elapsed().as_micros();
            match iteration % filters.len() {
                0 => assert_eq!(view.visible.len(), size),
                2 => assert_eq!(view.visible.len(), size / 2),
                3 | 4 => assert_eq!(view.visible.len(), size / 8),
                5 => assert!(view.visible.is_empty()),
                _ => {}
            }
            writeln!(
                output,
                "{size},filter,{iteration},{elapsed},{},{}",
                view.visible.len(),
                view.selected.len()
            )?;
        }
        selection.set_filter("")?;
        for iteration in 0..10 {
            selection.change(SelectionChange::Clear)?;
            let began = Instant::now();
            selection.change(SelectionChange::SelectMatches)?;
            let elapsed = began.elapsed().as_micros();
            let view = selection.view();
            assert_eq!(view.summary.selected_count, size);
            assert_eq!(
                view.summary.selected_bytes.get(),
                u64::try_from(size)? * 1024
            );
            writeln!(
                output,
                "{size},select_all,{iteration},{elapsed},{size},{size}"
            )?;
        }
        for iteration in 0..100 {
            let began = Instant::now();
            let view = black_box(selection.view());
            writeln!(
                output,
                "{size},snapshot,{iteration},{},{},{}",
                began.elapsed().as_micros(),
                view.visible.len(),
                view.selected.len()
            )?;
        }
    }
    output.flush()?;
    Ok(())
}
