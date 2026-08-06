//! Shared word-cloud stopwords + entity gazetteer, loaded from the osinttools
//! dashboard config directory (read-only in papr).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Default shared config dir used by both papr and osinttools.
pub const DEFAULT_DASHBOARD_DIR: &str = "/product/osinttools/data/dashboard";

pub const STOPWORDS_FILE: &str = "wordcloud-stopwords.json";
pub const ENTITIES_FILE: &str = "wordcloud-entities.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityGroup {
    Country,
    Person,
    Location,
    Military,
    Politics,
    Economy,
    Disaster,
    Org,
    General,
}

impl EntityGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Country => "country",
            Self::Person => "person",
            Self::Location => "location",
            Self::Military => "military",
            Self::Politics => "politics",
            Self::Economy => "economy",
            Self::Disaster => "disaster",
            Self::Org => "org",
            Self::General => "general",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "country" => Some(Self::Country),
            "person" => Some(Self::Person),
            "location" => Some(Self::Location),
            "military" => Some(Self::Military),
            "politics" => Some(Self::Politics),
            "economy" => Some(Self::Economy),
            "disaster" => Some(Self::Disaster),
            "org" => Some(Self::Org),
            "general" => Some(Self::General),
            _ => None,
        }
    }
}

impl Default for EntityGroup {
    fn default() -> Self {
        Self::General
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopwordsFile {
    pub version: i32,
    pub words: Vec<String>,
}

impl Default for StopwordsFile {
    fn default() -> Self {
        Self {
            version: 1,
            words: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordCloudEntity {
    pub id: String,
    pub canonical: String,
    pub group: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitiesFile {
    pub version: i32,
    pub entities: Vec<WordCloudEntity>,
}

impl Default for EntitiesFile {
    fn default() -> Self {
        Self {
            version: 1,
            entities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct AliasEntry {
    alias: String,
    entity_idx: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedEntity {
    pub id: String,
    pub canonical: String,
    pub group: EntityGroup,
}

/// In-memory gazetteer + stopwords used while building a cloud.
#[derive(Debug, Clone)]
pub struct WordCloudDict {
    pub stopwords: StopwordsFile,
    pub entities: EntitiesFile,
    file_stopwords: HashSet<String>,
    aliases: Vec<AliasEntry>,
    by_id: HashMap<String, ResolvedEntity>,
    pub dir: PathBuf,
}

impl WordCloudDict {
    pub fn empty(dir: PathBuf) -> Self {
        Self {
            stopwords: StopwordsFile::default(),
            entities: EntitiesFile::default(),
            file_stopwords: HashSet::new(),
            aliases: Vec::new(),
            by_id: HashMap::new(),
            dir,
        }
    }

    pub fn resolve_dir() -> PathBuf {
        if let Ok(p) = std::env::var("PAPR_WORDCLOUD_DIR") {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
        PathBuf::from(DEFAULT_DASHBOARD_DIR)
    }

    pub fn load_from_dir(dir: &Path) -> Self {
        let mut dict = Self::empty(dir.to_path_buf());
        dict.reload();
        dict
    }

    pub fn load_default() -> Self {
        Self::load_from_dir(&Self::resolve_dir())
    }

    pub fn reload(&mut self) {
        self.load_stopwords();
        self.load_entities();
    }

    fn load_stopwords(&mut self) {
        let path = self.dir.join(STOPWORDS_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<StopwordsFile>(&raw) {
                Ok(mut doc) => {
                    if doc.version == 0 {
                        doc.version = 1;
                    }
                    doc.words = normalize_stopwords(doc.words);
                    self.apply_stopwords(doc);
                }
                Err(e) => {
                    log::warn!(
                        "wordcloud stopwords parse failed ({}): {e}",
                        path.display()
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.apply_stopwords(StopwordsFile::default());
            }
            Err(e) => {
                log::warn!(
                    "wordcloud stopwords read failed ({}): {e}",
                    path.display()
                );
            }
        }
    }

    fn load_entities(&mut self) {
        let path = self.dir.join(ENTITIES_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<EntitiesFile>(&raw) {
                Ok(mut doc) => {
                    if doc.version == 0 {
                        doc.version = 1;
                    }
                    if doc.entities.is_empty() && doc.entities.capacity() == 0 {
                        // keep empty
                    }
                    self.apply_entities(doc);
                }
                Err(e) => {
                    log::warn!(
                        "wordcloud entities parse failed ({}): {e}",
                        path.display()
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.apply_entities(EntitiesFile::default());
            }
            Err(e) => {
                log::warn!(
                    "wordcloud entities read failed ({}): {e}",
                    path.display()
                );
            }
        }
    }

    fn apply_stopwords(&mut self, doc: StopwordsFile) {
        let mut set = HashSet::with_capacity(doc.words.len());
        for w in &doc.words {
            set.insert(w.clone());
        }
        self.file_stopwords = set;
        self.stopwords = doc;
    }

    fn apply_entities(&mut self, doc: EntitiesFile) {
        let mut by_id = HashMap::new();
        let mut aliases = Vec::new();

        for (idx, ent) in doc.entities.iter().enumerate() {
            let id = ent.id.trim();
            let canonical = ent.canonical.trim();
            if id.is_empty() || canonical.is_empty() {
                continue;
            }
            let group = EntityGroup::parse(&ent.group).unwrap_or(EntityGroup::General);
            by_id.insert(
                id.to_string(),
                ResolvedEntity {
                    id: id.to_string(),
                    canonical: canonical.to_string(),
                    group,
                },
            );

            let mut seen = HashSet::new();
            let mut push_alias = |raw: &str| {
                let n = normalize_alias(raw);
                if n.is_empty() || !seen.insert(n.clone()) {
                    return;
                }
                aliases.push(AliasEntry {
                    alias: n,
                    entity_idx: idx,
                });
            };
            push_alias(canonical);
            for a in &ent.aliases {
                push_alias(a);
            }
        }

        // Longest alias first so "united states" wins over "us".
        aliases.sort_by(|a, b| b.alias.len().cmp(&a.alias.len()).then_with(|| a.alias.cmp(&b.alias)));

        self.by_id = by_id;
        self.aliases = aliases;
        self.entities = doc;
    }

    pub fn is_file_stopword(&self, term: &str) -> bool {
        self.file_stopwords.contains(term)
    }

    /// Longest-alias entity match. Returns hit counts keyed by entity id,
    /// a byte occupancy mask over the lowercased text, and that normalized text.
    pub fn match_entities(&self, text: &str) -> (HashMap<String, i64>, Vec<bool>, String) {
        let norm = text.to_lowercase();
        let mut occupied = vec![false; norm.len()];
        let mut hits: HashMap<String, i64> = HashMap::new();
        if self.aliases.is_empty() || norm.is_empty() {
            return (hits, occupied, norm);
        }

        for ae in &self.aliases {
            let alias = &ae.alias;
            if alias.is_empty() {
                continue;
            }
            let Some(ent) = self.entities.entities.get(ae.entity_idx) else {
                continue;
            };
            let id = ent.id.trim();
            if id.is_empty() {
                continue;
            }

            let mut search_from = 0;
            while search_from + alias.len() <= norm.len() {
                let rest = &norm[search_from..];
                let Some(rel) = rest.find(alias.as_str()) else {
                    break;
                };
                let start = search_from + rel;
                let end = start + alias.len();
                if span_occupied(&occupied, start, end) || !alias_boundary_ok(&norm, alias, start, end)
                {
                    search_from = start + 1;
                    continue;
                }
                for slot in occupied.iter_mut().take(end).skip(start) {
                    *slot = true;
                }
                *hits.entry(id.to_string()).or_insert(0) += 1;
                search_from = end;
            }
        }
        (hits, occupied, norm)
    }

    pub fn entity(&self, id: &str) -> Option<&ResolvedEntity> {
        self.by_id.get(id)
    }
}

/// Process-wide dict with cheap reload for shared JSON files.
pub struct SharedWordCloudDict {
    inner: RwLock<WordCloudDict>,
}

/// Optional process-wide handle installed by the server (and used by ingest
/// indexing inside `upsert_article`). Falls back to a lazily loaded default.
static PROCESS_DICT: OnceLock<Arc<SharedWordCloudDict>> = OnceLock::new();

/// Install the shared dict used by ingest + background backfill.
/// Idempotent: the first call wins (server startup).
pub fn install_process_dict(dict: Arc<SharedWordCloudDict>) {
    let _ = PROCESS_DICT.set(dict);
}

/// Process-wide dictionary for ingest indexing when no explicit dict is passed.
pub fn process_dict() -> Arc<SharedWordCloudDict> {
    PROCESS_DICT
        .get_or_init(|| Arc::new(SharedWordCloudDict::load_default()))
        .clone()
}

impl SharedWordCloudDict {
    pub fn load_default() -> Self {
        let dict = WordCloudDict::load_default();
        log::info!(
            "wordcloud dict loaded from {} (stopwords={}, entities={})",
            dict.dir.display(),
            dict.stopwords.words.len(),
            dict.entities.entities.len()
        );
        Self {
            inner: RwLock::new(dict),
        }
    }

    pub fn with_dict<R>(&self, f: impl FnOnce(&WordCloudDict) -> R) -> R {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    pub fn reload(&self) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        guard.reload();
        log::info!(
            "wordcloud dict reloaded from {} (stopwords={}, entities={})",
            guard.dir.display(),
            guard.stopwords.words.len(),
            guard.entities.entities.len()
        );
        // Preset cloud cache is keyed by day window only — drop it so the next
        // request rebuilds against the new gazetteer/stopwords. Term rows stay
        // until backfill rewrites them for the bumped dict version.
        crate::wordcloud::invalidate_cache();
    }

    pub fn snapshot_stopwords(&self) -> StopwordsFile {
        self.with_dict(|d| d.stopwords.clone())
    }

    pub fn snapshot_entities(&self) -> EntitiesFile {
        self.with_dict(|d| d.entities.clone())
    }
}

fn normalize_stopwords(words: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for raw in words {
        let mut w = raw.trim().to_string();
        if w.is_empty() {
            continue;
        }
        if w.is_ascii() {
            w = w.to_lowercase();
        }
        if !seen.insert(w.clone()) {
            continue;
        }
        out.push(w);
    }
    out
}

fn normalize_alias(raw: &str) -> String {
    raw.trim().to_lowercase()
}

fn span_occupied(occupied: &[bool], start: usize, end: usize) -> bool {
    occupied[start..end].iter().any(|b| *b)
}

fn contains_han(s: &str) -> bool {
    s.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

fn alias_boundary_ok(norm: &str, alias: &str, start: usize, end: usize) -> bool {
    if contains_han(alias) {
        return true;
    }
    if start > 0 {
        if let Some(c) = norm[..start].chars().next_back() {
            if c.is_ascii_alphanumeric() {
                return false;
            }
        }
    }
    if end < norm.len() {
        if let Some(c) = norm[end..].chars().next() {
            if c.is_ascii_alphanumeric() {
                return false;
            }
        }
    }
    true
}

/// Contiguous free substrings of `norm` (for leftover tokenization).
pub fn unoccupied_spans(norm: &str, occupied: &[bool]) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = norm.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && occupied[i] {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let j = (i..bytes.len()).find(|&k| occupied[k]).unwrap_or(bytes.len());
        // Only slice at char boundaries — occupied is byte-indexed over UTF-8.
        let span = norm.get(i..j).unwrap_or("").trim();
        if !span.is_empty() {
            out.push(span.to_string());
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_alias_wins() {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![WordCloudEntity {
                id: "country.us".into(),
                canonical: "United States".into(),
                group: "country".into(),
                aliases: vec!["us".into(), "united states".into()],
            }],
        });
        let (hits, occupied, _) = dict.match_entities("the united states and us met");
        assert_eq!(hits.get("country.us").copied().unwrap_or(0), 2);
        assert!(occupied.iter().any(|b| *b));
    }
}
