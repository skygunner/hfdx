//! Shared catalog and selection state.
use crate::repository::{
    Bytes, CatalogEntry, CatalogId, CatalogPage, CatalogRequest, EntryId, RepositorySpec,
};
use globset::{GlobBuilder, GlobMatcher};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Catalog {
    pub id: CatalogId,
    pub source: RepositorySpec,
    pages: Vec<(usize, Arc<[CatalogEntry]>)>,
    len: usize,
    pub(crate) profiles: std::sync::OnceLock<Vec<crate::profiles::Profile>>,
}
impl Catalog {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn entry(&self, id: EntryId) -> Option<&CatalogEntry> {
        if id.catalog != self.id {
            return None;
        }
        let index = usize::try_from(id.index).ok()?;
        let page = self
            .pages
            .partition_point(|(start, _)| *start <= index)
            .checked_sub(1)?;
        let (start, entries) = self.pages.get(page)?;
        entries.get(index - start)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogState {
    Empty,
    Loading,
    Ready,
    Failed,
}
#[derive(Clone, Debug)]
pub enum SelectionChange {
    Set { id: EntryId, selected: bool },
    Group { ids: Vec<EntryId>, selected: bool },
    SelectMatches,
    DeselectMatches,
    Clear,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionSummary {
    pub selected_count: usize,
    pub selected_bytes: Bytes,
    pub hidden_selected_count: usize,
}
#[derive(Clone, Debug)]
pub struct SelectionView {
    pub catalog: Option<Arc<Catalog>>,
    pub state: CatalogState,
    pub filter: String,
    pub selected_only: bool,
    pub matches: Arc<[EntryId]>,
    pub visible: Arc<[EntryId]>,
    pub selected: Arc<BTreeSet<EntryId>>,
    pub summary: SelectionSummary,
}
#[derive(Clone, Debug)]
pub struct FrozenSelection {
    pub source: RepositorySpec,
    pub files: Vec<CatalogEntry>,
    pub total_bytes: Bytes,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    NotReady,
    StaleEntry,
    EmptySelection,
    InvalidFilter,
    SizeOverflow,
    ConflictingEntry,
}
impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SelectionError {}

enum Pattern {
    Text(String),
    Glob(GlobMatcher, Vec<char>),
}
impl Pattern {
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Text(text) => path.to_ascii_lowercase().contains(text),
            Self::Glob(glob, pattern) => {
                if !glob.is_match(path) {
                    return false;
                }
                let text: Vec<char> = path.chars().collect();
                let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
                while t < text.len() {
                    if p < pattern.len()
                        && (pattern[p] == '?' || pattern[p].eq_ignore_ascii_case(&text[t]))
                        && pattern[p] != '*'
                    {
                        p += 1;
                        t += 1;
                    } else if p < pattern.len() && pattern[p] == '*' {
                        star = Some(p);
                        p += 1;
                        retry = t;
                    } else if let Some(index) = star {
                        retry += 1;
                        t = retry;
                        p = index + 1;
                    } else {
                        return false;
                    }
                }
                while p < pattern.len() && pattern[p] == '*' {
                    p += 1;
                }
                p == pattern.len()
            }
        }
    }
}
pub struct Selection {
    next_id: u64,
    catalog: Option<Arc<Catalog>>,
    state: CatalogState,
    paths: BTreeMap<String, EntryId>,
    filter: String,
    patterns: Vec<Pattern>,
    selected_only: bool,
    matches: Arc<[EntryId]>,
    visible: Arc<[EntryId]>,
    selected: Arc<BTreeSet<EntryId>>,
    selected_bytes: u64,
    hidden: usize,
}
impl Default for Selection {
    fn default() -> Self {
        Self::new()
    }
}
impl Selection {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            catalog: None,
            state: CatalogState::Empty,
            paths: BTreeMap::new(),
            filter: String::new(),
            patterns: Vec::new(),
            selected_only: false,
            matches: Arc::from([]),
            visible: Arc::from([]),
            selected: Arc::new(BTreeSet::new()),
            selected_bytes: 0,
            hidden: 0,
        }
    }
    pub fn reset(&mut self) {
        let next_id = self.next_id;
        *self = Self::new();
        self.next_id = next_id;
    }
    pub fn begin(&mut self, source: RepositorySpec) -> Result<CatalogRequest, SelectionError> {
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(SelectionError::SizeOverflow)?;
        self.reset();
        self.next_id = next;
        let id = CatalogId(next);
        self.catalog = Some(Arc::new(Catalog {
            profiles: Default::default(),
            id,
            source: source.clone(),
            pages: Vec::new(),
            len: 0,
        }));
        self.state = CatalogState::Loading;
        Ok(CatalogRequest { id, source })
    }
    pub fn append(&mut self, page: CatalogPage) -> Result<(), SelectionError> {
        let catalog = self.catalog.as_ref().ok_or(SelectionError::StaleEntry)?;
        if catalog.id != page.catalog {
            return Err(SelectionError::StaleEntry);
        }
        if self.state != CatalogState::Loading {
            return Err(SelectionError::NotReady);
        }
        let mut unique = Vec::new();
        let mut pending = BTreeMap::<String, usize>::new();
        for entry in page.entries {
            let path = entry.path.as_str();
            if let Some(id) = self.paths.get(path) {
                if catalog.entry(*id) != Some(&entry) {
                    return Err(SelectionError::ConflictingEntry);
                }
            } else if let Some(index) = pending.get(path) {
                if unique.get(*index) != Some(&entry) {
                    return Err(SelectionError::ConflictingEntry);
                }
            } else {
                pending.insert(path.to_owned(), unique.len());
                unique.push(entry);
            }
        }
        let end = catalog
            .len
            .checked_add(unique.len())
            .ok_or(SelectionError::SizeOverflow)?;
        if end > usize::try_from(u32::MAX).unwrap_or(usize::MAX) {
            return Err(SelectionError::SizeOverflow);
        }
        let start = catalog.len;
        let mut new_matches = Vec::new();
        for (offset, entry) in unique.iter().enumerate() {
            let index = u32::try_from(start + offset).map_err(|_| SelectionError::SizeOverflow)?;
            let id = EntryId {
                catalog: page.catalog,
                index,
            };
            self.paths.insert(entry.path.as_str().to_owned(), id);
            if self.patterns.is_empty()
                || self.patterns.iter().any(|p| p.matches(entry.path.as_str()))
            {
                new_matches.push(id);
            }
        }
        if !unique.is_empty() {
            let catalog = Arc::make_mut(self.catalog.as_mut().ok_or(SelectionError::StaleEntry)?);
            catalog.profiles.take();
            catalog.pages.push((start, Arc::from(unique)));
            catalog.len = end;
        }
        if !new_matches.is_empty() {
            let mut matches = Vec::with_capacity(self.matches.len() + new_matches.len());
            matches.extend_from_slice(&self.matches);
            matches.extend(new_matches);
            self.matches = Arc::from(matches);
            if !self.selected_only {
                self.visible = self.matches.clone();
            }
        }
        if page.complete {
            self.state = CatalogState::Ready;
        }
        Ok(())
    }
    pub fn fail(&mut self, catalog: CatalogId) {
        if self.catalog.as_ref().is_some_and(|c| c.id == catalog)
            && self.state == CatalogState::Loading
        {
            self.state = CatalogState::Failed;
        }
    }
    pub fn set_filter(&mut self, input: &str) -> Result<(), SelectionError> {
        let mut patterns = Vec::new();
        for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if line.contains(['*', '?']) {
                let mut glob = String::new();
                let mut previous_wildcard = false;
                for ch in line.chars() {
                    if ch == '*' || ch == '?' {
                        if !previous_wildcard {
                            glob.push('*');
                        }
                        previous_wildcard = true;
                    } else {
                        glob.push_str(&globset::escape(&ch.to_string()));
                        previous_wildcard = false;
                    }
                }
                let matcher = GlobBuilder::new(&glob)
                    .literal_separator(false)
                    .backslash_escape(true)
                    .case_insensitive(true)
                    .build()
                    .map_err(|_| SelectionError::InvalidFilter)?
                    .compile_matcher();
                patterns.push(Pattern::Glob(matcher, line.chars().collect()));
            } else {
                patterns.push(Pattern::Text(line.to_ascii_lowercase()));
            }
        }
        self.filter = input.to_owned();
        self.patterns = patterns;
        self.refresh();
        Ok(())
    }
    pub fn set_selected_only(&mut self, enabled: bool) {
        self.selected_only = enabled;
        self.refresh_visible();
    }
    pub fn change(&mut self, change: SelectionChange) -> Result<(), SelectionError> {
        let catalog = self.catalog.as_ref().ok_or(SelectionError::NotReady)?;
        if !matches!(self.state, CatalogState::Loading | CatalogState::Ready) {
            return Err(SelectionError::NotReady);
        }
        if let SelectionChange::Set {
            id,
            selected: value,
        } = change
        {
            let entry = catalog.entry(id).ok_or(SelectionError::StaleEntry)?;
            let was_selected = self.selected.contains(&id);
            if value != was_selected {
                let total = if value {
                    self.selected_bytes
                        .checked_add(entry.size.get())
                        .ok_or(SelectionError::SizeOverflow)?
                } else {
                    self.selected_bytes - entry.size.get()
                };
                let selected = Arc::make_mut(&mut self.selected);
                if value {
                    selected.insert(id);
                } else {
                    selected.remove(&id);
                }
                self.selected_bytes = total;
                if self.matches.binary_search(&id).is_err() {
                    if value {
                        self.hidden += 1;
                    } else {
                        self.hidden -= 1;
                    }
                }
                if self.selected_only {
                    self.visible = Arc::from(
                        self.matches
                            .iter()
                            .copied()
                            .filter(|id| self.selected.contains(id))
                            .collect::<Vec<_>>(),
                    );
                }
            }
            return Ok(());
        }
        let mut selected = (*self.selected).clone();
        match change {
            SelectionChange::Set { .. } => unreachable!(),
            SelectionChange::Group {
                ids,
                selected: value,
            } => {
                if self.state != CatalogState::Ready {
                    return Err(SelectionError::NotReady);
                }
                for id in ids {
                    catalog.entry(id).ok_or(SelectionError::StaleEntry)?;
                    if value {
                        selected.insert(id);
                    } else {
                        selected.remove(&id);
                    }
                }
            }
            SelectionChange::SelectMatches => {
                if self.state != CatalogState::Ready {
                    return Err(SelectionError::NotReady);
                }
                selected.extend(self.matches.iter().copied());
            }
            SelectionChange::DeselectMatches => {
                if self.state != CatalogState::Ready {
                    return Err(SelectionError::NotReady);
                }
                for id in self.matches.iter() {
                    selected.remove(id);
                }
            }
            SelectionChange::Clear => {
                if self.state != CatalogState::Ready {
                    return Err(SelectionError::NotReady);
                }
                selected.clear();
            }
        }
        let mut total = 0u64;
        for id in &selected {
            let entry = catalog.entry(*id).ok_or(SelectionError::StaleEntry)?;
            total = total
                .checked_add(entry.size.get())
                .ok_or(SelectionError::SizeOverflow)?;
        }
        self.selected = Arc::new(selected);
        self.selected_bytes = total;
        self.refresh_visible();
        Ok(())
    }
    pub fn view(&self) -> SelectionView {
        SelectionView {
            catalog: self.catalog.clone(),
            state: self.state,
            filter: self.filter.clone(),
            selected_only: self.selected_only,
            matches: self.matches.clone(),
            visible: self.visible.clone(),
            selected: self.selected.clone(),
            summary: SelectionSummary {
                selected_count: self.selected.len(),
                selected_bytes: Bytes::new(self.selected_bytes),
                hidden_selected_count: self.hidden,
            },
        }
    }
    pub fn freeze(&self) -> Result<FrozenSelection, SelectionError> {
        if self.state != CatalogState::Ready {
            return Err(SelectionError::NotReady);
        }
        if self.selected.is_empty() {
            return Err(SelectionError::EmptySelection);
        }
        let catalog = self.catalog.as_ref().ok_or(SelectionError::NotReady)?;
        let files = self
            .selected
            .iter()
            .map(|id| {
                catalog
                    .entry(*id)
                    .cloned()
                    .ok_or(SelectionError::StaleEntry)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FrozenSelection {
            source: catalog.source.clone(),
            files,
            total_bytes: Bytes::new(self.selected_bytes),
        })
    }
    fn refresh(&mut self) {
        let mut matches = Vec::new();
        if let Some(catalog) = &self.catalog {
            for (start, entries) in &catalog.pages {
                for (offset, entry) in entries.iter().enumerate() {
                    if (self.patterns.is_empty()
                        || self.patterns.iter().any(|p| p.matches(entry.path.as_str())))
                        && let Ok(index) = u32::try_from(start + offset)
                    {
                        matches.push(EntryId {
                            catalog: catalog.id,
                            index,
                        });
                    }
                }
            }
        }
        self.matches = Arc::from(matches);
        self.refresh_visible();
    }
    fn refresh_visible(&mut self) {
        let matched_selected = self
            .matches
            .iter()
            .filter(|id| self.selected.contains(id))
            .count();
        self.hidden = self.selected.len() - matched_selected;
        self.visible = if self.selected_only {
            Arc::from(
                self.matches
                    .iter()
                    .copied()
                    .filter(|id| self.selected.contains(id))
                    .collect::<Vec<_>>(),
            )
        } else {
            self.matches.clone()
        };
    }
}
