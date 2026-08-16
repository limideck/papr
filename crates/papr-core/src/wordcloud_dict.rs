//! Shared word-cloud stopwords + entity gazetteer.
//!
//! Load priority for entities:
//! 1. Explicit `PAPR_WORDCLOUD_DIR` (when set)
//! 2. Local copy-on-write file next to `PAPR_DB` (or `PAPR_WORDCLOUD_COW_DIR`)
//! 3. Shared osinttools seed (`DEFAULT_DASHBOARD_DIR`)
//!
//! Writes never touch the shared seed; the first edit copies entities into the
//! papr-owned COW path (unless `PAPR_WORDCLOUD_DIR` points at a non-seed dir).

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Default shared config dir used by both papr and osinttools.
pub const DEFAULT_DASHBOARD_DIR: &str = "/product/osinttools/data/dashboard";

pub const STOPWORDS_FILE: &str = "wordcloud-stopwords.json";
pub const ENTITIES_FILE: &str = "wordcloud-entities.json";

/// Where the active entities JSON was loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntitiesSource {
    /// `PAPR_WORDCLOUD_DIR` (non-seed) — readable and writable there.
    Explicit,
    /// Papr-owned overlay under the data dir / `PAPR_WORDCLOUD_COW_DIR`.
    Local,
    /// Shared osinttools seed — read-only; edits trigger COW.
    Shared,
}

impl EntitiesSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Local => "local",
            Self::Shared => "shared",
        }
    }
}

/// Metadata returned to admins about the active entities file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitiesFileMeta {
    pub source: EntitiesSource,
    pub path: String,
    pub writable: bool,
    pub seed_dir: String,
    pub cow_dir: String,
}

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

/// Synonym set for search-time FTS expansion (canonical + aliases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynonymGroup {
    pub id: String,
    pub canonical: String,
    /// Deduped surface forms (canonical first) for FTS quoting / highlighting.
    pub aliases: Vec<String>,
}

/// In-memory gazetteer + stopwords used while building a cloud.
#[derive(Debug, Clone)]
pub struct WordCloudDict {
    pub stopwords: StopwordsFile,
    pub entities: EntitiesFile,
    file_stopwords: HashSet<String>,
    aliases: Vec<AliasEntry>,
    /// Exact normalized alias → entity index (for search synonym lookup).
    alias_exact: HashMap<String, usize>,
    by_id: HashMap<String, ResolvedEntity>,
    /// Active entities directory (seed, COW, or explicit). Kept for logs/compat.
    pub dir: PathBuf,
    /// Shared / explicit seed directory (stopwords always load from here).
    pub seed_dir: PathBuf,
    /// Papr-owned overlay directory for entity edits.
    pub cow_dir: PathBuf,
    pub entities_source: EntitiesSource,
}

impl WordCloudDict {
    pub fn empty(dir: PathBuf) -> Self {
        let cow = resolve_cow_dir();
        Self {
            stopwords: StopwordsFile::default(),
            entities: EntitiesFile::default(),
            file_stopwords: HashSet::new(),
            aliases: Vec::new(),
            alias_exact: HashMap::new(),
            by_id: HashMap::new(),
            seed_dir: dir.clone(),
            cow_dir: cow,
            entities_source: EntitiesSource::Shared,
            dir,
        }
    }

    /// Seed / explicit config directory (stopwords + default entities).
    pub fn resolve_dir() -> PathBuf {
        resolve_seed_dir()
    }

    pub fn load_from_dir(dir: &Path) -> Self {
        let mut dict = Self::empty(dir.to_path_buf());
        dict.seed_dir = dir.to_path_buf();
        dict.cow_dir = resolve_cow_dir();
        dict.reload();
        dict
    }

    pub fn load_default() -> Self {
        Self::load_from_dir(&Self::resolve_dir())
    }

    pub fn entities_path(&self) -> PathBuf {
        self.dir.join(ENTITIES_FILE)
    }

    pub fn entities_meta(&self) -> EntitiesFileMeta {
        let path = self.entities_path();
        let writable = match self.entities_source {
            EntitiesSource::Shared => true, // first edit will COW
            EntitiesSource::Local | EntitiesSource::Explicit => true,
        };
        EntitiesFileMeta {
            source: self.entities_source,
            path: path.display().to_string(),
            writable,
            seed_dir: self.seed_dir.display().to_string(),
            cow_dir: self.cow_dir.display().to_string(),
        }
    }

    pub fn reload(&mut self) {
        self.load_stopwords();
        self.load_entities();
    }

    fn load_stopwords(&mut self) {
        let path = self.seed_dir.join(STOPWORDS_FILE);
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
        let (path, source) = resolve_entities_load_path(&self.seed_dir, &self.cow_dir);
        self.dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone());
        self.entities_source = source;
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<EntitiesFile>(&raw) {
                Ok(mut doc) => {
                    if doc.version == 0 {
                        doc.version = 1;
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

    /// Update an entity's display canonical (and optionally aliases).
    ///
    /// On first write against the shared seed, copies the entities file into
    /// the papr COW directory. Never writes the shared osinttools seed.
    ///
    /// When the canonical changes, the previous canonical is retained as an
    /// alias (unless already present) so matching / synonyms keep working.
    pub fn update_entity(
        &mut self,
        id: &str,
        canonical: Option<&str>,
        aliases: Option<Vec<String>>,
    ) -> Result<WordCloudEntity, AppError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(AppError::code("entityNotFound"));
        }
        let idx = self
            .entities
            .entities
            .iter()
            .position(|e| e.id.trim() == id)
            .ok_or_else(|| AppError::code("entityNotFound"))?;

        let old_canonical = self.entities.entities[idx].canonical.clone();
        let mut changed = false;

        if let Some(raw) = canonical {
            let next = raw.trim();
            if next.is_empty() {
                return Err(AppError::code("emptyCanonical"));
            }
            if next != self.entities.entities[idx].canonical {
                // Keep the previous display form as an alias when casing/spelling changes.
                retain_previous_canonical_as_alias(
                    &mut self.entities.entities[idx].aliases,
                    &old_canonical,
                    next,
                );
                self.entities.entities[idx].canonical = next.to_string();
                changed = true;
            }
        }

        if let Some(list) = aliases {
            let cleaned = clean_aliases(list);
            if cleaned != self.entities.entities[idx].aliases {
                self.entities.entities[idx].aliases = cleaned;
                // After an explicit aliases replace, still keep the pre-edit
                // canonical when the display name changed in this same call.
                if let Some(raw) = canonical {
                    let next = raw.trim();
                    if !next.is_empty() && next != old_canonical.trim() {
                        retain_previous_canonical_as_alias(
                            &mut self.entities.entities[idx].aliases,
                            &old_canonical,
                            next,
                        );
                    }
                }
                changed = true;
            }
        }

        if !changed {
            return Ok(self.entities.entities[idx].clone());
        }

        self.entities.version = self.entities.version.saturating_add(1).max(1);
        let path = self.ensure_entities_writable()?;
        write_entities_file(&path, &self.entities)?;
        // Re-index from the mutated document.
        let doc = self.entities.clone();
        self.apply_entities(doc);
        Ok(self.entities.entities[idx].clone())
    }

    /// Create a new entity (promote a residual cloud token, or add a gazetteer entry).
    ///
    /// When `id` is empty/None, generates `{group}.{slug}` from the canonical
    /// (with a numeric suffix on collision). First write against the shared seed
    /// triggers COW. Canonical is indexed for matching via lowercase normalize,
    /// so display `AI` still matches body text `ai` after backfill.
    pub fn create_entity(
        &mut self,
        id: Option<&str>,
        canonical: &str,
        group: Option<&str>,
        aliases: Option<Vec<String>>,
    ) -> Result<WordCloudEntity, AppError> {
        let canonical = canonical.trim();
        if canonical.is_empty() {
            return Err(AppError::code("emptyCanonical"));
        }
        let group = EntityGroup::parse(group.unwrap_or("general"))
            .unwrap_or(EntityGroup::General)
            .as_str()
            .to_string();

        let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => {
                if self.entities.entities.iter().any(|e| e.id.trim() == raw) {
                    return Err(AppError::code("entityIdExists"));
                }
                raw.to_string()
            }
            None => suggest_entity_id(&group, canonical, &self.entities.entities),
        };

        let mut aliases = clean_aliases(aliases.unwrap_or_default());
        // Keep a lowercase residual surface when casing differs (ai → AI).
        retain_previous_canonical_as_alias(&mut aliases, &canonical.to_lowercase(), canonical);

        let ent = WordCloudEntity {
            id,
            canonical: canonical.to_string(),
            group,
            aliases,
        };
        self.entities.entities.push(ent.clone());
        self.entities.version = self.entities.version.saturating_add(1).max(1);
        let path = self.ensure_entities_writable()?;
        write_entities_file(&path, &self.entities)?;
        let doc = self.entities.clone();
        self.apply_entities(doc);
        Ok(ent)
    }

    /// Ensure entities are written under a papr-owned (or explicit non-seed) path.
    fn ensure_entities_writable(&mut self) -> Result<PathBuf, AppError> {
        match self.entities_source {
            EntitiesSource::Explicit => {
                let path = self.seed_dir.join(ENTITIES_FILE);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        AppError::other(format!(
                            "create wordcloud dir {}: {e}",
                            parent.display()
                        ))
                    })?;
                }
                Ok(path)
            }
            EntitiesSource::Local => {
                std::fs::create_dir_all(&self.cow_dir).map_err(|e| {
                    AppError::other(format!(
                        "create wordcloud COW dir {}: {e}",
                        self.cow_dir.display()
                    ))
                })?;
                Ok(self.cow_dir.join(ENTITIES_FILE))
            }
            EntitiesSource::Shared => {
                // Copy-on-write: never mutate the shared seed.
                std::fs::create_dir_all(&self.cow_dir).map_err(|e| {
                    AppError::other(format!(
                        "create wordcloud COW dir {}: {e}",
                        self.cow_dir.display()
                    ))
                })?;
                let dest = self.cow_dir.join(ENTITIES_FILE);
                let src = self.seed_dir.join(ENTITIES_FILE);
                if !dest.exists() {
                    if src.exists() {
                        std::fs::copy(&src, &dest).map_err(|e| {
                            AppError::other(format!(
                                "copy wordcloud entities {} → {}: {e}",
                                src.display(),
                                dest.display()
                            ))
                        })?;
                        log::info!(
                            "wordcloud entities COW: copied {} → {}",
                            src.display(),
                            dest.display()
                        );
                    } else {
                        // No seed file — persist current in-memory doc.
                        write_entities_file(&dest, &self.entities)?;
                        log::info!(
                            "wordcloud entities COW: wrote in-memory seed to {}",
                            dest.display()
                        );
                    }
                }
                self.entities_source = EntitiesSource::Local;
                self.dir = self.cow_dir.clone();
                Ok(dest)
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

    /// Rebuild gazetteer indexes from an entities document (also used by tests).
    pub(crate) fn apply_entities(&mut self, doc: EntitiesFile) {
        let mut by_id = HashMap::new();
        let mut aliases = Vec::new();
        let mut alias_exact: HashMap<String, usize> = HashMap::new();

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
                    alias: n.clone(),
                    entity_idx: idx,
                });
                // Exact lookup: on shared aliases, keep deterministic winner
                // (lexicographically smaller entity id) and warn.
                match alias_exact.get(&n).copied() {
                    Some(prev_idx) => {
                        let prev_id = doc.entities[prev_idx].id.trim();
                        let new_id = id;
                        if new_id < prev_id {
                            log::warn!(
                                "wordcloud alias conflict for {n:?}: preferring {new_id} over {prev_id}"
                            );
                            alias_exact.insert(n, idx);
                        } else if new_id != prev_id {
                            log::warn!(
                                "wordcloud alias conflict for {n:?}: keeping {prev_id} over {new_id}"
                            );
                        }
                    }
                    None => {
                        alias_exact.insert(n, idx);
                    }
                }
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
        self.alias_exact = alias_exact;
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

    /// Exact alias → synonym group for search expansion (not substring match).
    ///
    /// Uses the same normalize rules as the gazetteer (`trim` + lowercase).
    /// Shared aliases across entities resolve to the lexicographically smaller
    /// entity id (see `apply_entities`).
    pub fn lookup_synonym_group(&self, term: &str) -> Option<SynonymGroup> {
        let key = normalize_alias(term);
        if key.is_empty() {
            return None;
        }
        let idx = *self.alias_exact.get(&key)?;
        let ent = self.entities.entities.get(idx)?;
        let id = ent.id.trim();
        let canonical = ent.canonical.trim();
        if id.is_empty() || canonical.is_empty() {
            return None;
        }
        Some(SynonymGroup {
            id: id.to_string(),
            canonical: canonical.to_string(),
            aliases: synonym_surfaces(ent),
        })
    }
}

/// Deduped canonical + aliases (preserve first-seen casing; skip empties).
fn synonym_surfaces(ent: &WordCloudEntity) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let push = |raw: &str, seen: &mut HashSet<String>, out: &mut Vec<String>| {
        let t = raw.trim();
        if t.is_empty() {
            return;
        }
        let key = normalize_alias(t);
        if key.is_empty() || !seen.insert(key) {
            return;
        }
        out.push(t.to_string());
    };
    push(&ent.canonical, &mut seen, &mut out);
    for a in &ent.aliases {
        push(a, &mut seen, &mut out);
    }
    out
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
            "wordcloud dict loaded from {} (source={}, stopwords={}, entities={})",
            dict.dir.display(),
            dict.entities_source.as_str(),
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
            "wordcloud dict reloaded from {} (source={}, stopwords={}, entities={})",
            guard.dir.display(),
            guard.entities_source.as_str(),
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

    pub fn entities_meta(&self) -> EntitiesFileMeta {
        self.with_dict(|d| d.entities_meta())
    }

    /// Patch one entity, persist via COW, and refresh in-memory indexes.
    pub fn update_entity(
        &self,
        id: &str,
        canonical: Option<&str>,
        aliases: Option<Vec<String>>,
    ) -> Result<(WordCloudEntity, EntitiesFileMeta), AppError> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let ent = guard.update_entity(id, canonical, aliases)?;
        let meta = guard.entities_meta();
        crate::wordcloud::invalidate_cache();
        Ok((ent, meta))
    }

    /// Create an entity (residual promote / new gazetteer row), COW + cache bust.
    pub fn create_entity(
        &self,
        id: Option<&str>,
        canonical: &str,
        group: Option<&str>,
        aliases: Option<Vec<String>>,
    ) -> Result<(WordCloudEntity, EntitiesFileMeta), AppError> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let ent = guard.create_entity(id, canonical, group, aliases)?;
        let meta = guard.entities_meta();
        crate::wordcloud::invalidate_cache();
        Ok((ent, meta))
    }
}

/// Seed directory: `PAPR_WORDCLOUD_DIR` or the shared osinttools dashboard.
pub fn resolve_seed_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PAPR_WORDCLOUD_DIR") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    PathBuf::from(DEFAULT_DASHBOARD_DIR)
}

/// Papr-owned overlay for entity edits.
///
/// Priority: `PAPR_WORDCLOUD_COW_DIR` → sibling of `PAPR_DB` named `wordcloud`
/// → `./wordcloud`.
pub fn resolve_cow_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PAPR_WORDCLOUD_COW_DIR") {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(db) = std::env::var("PAPR_DB") {
        let trimmed = db.trim();
        if !trimmed.is_empty() {
            let db_path = PathBuf::from(trimmed);
            if let Some(parent) = db_path.parent() {
                if !parent.as_os_str().is_empty() {
                    return parent.join("wordcloud");
                }
            }
            return PathBuf::from("wordcloud");
        }
    }
    PathBuf::from("wordcloud")
}

fn paths_equal_relaxed(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

fn is_shared_seed_dir(dir: &Path) -> bool {
    paths_equal_relaxed(dir, Path::new(DEFAULT_DASHBOARD_DIR))
}

/// Resolve which entities JSON to load and its source label.
fn resolve_entities_load_path(seed_dir: &Path, cow_dir: &Path) -> (PathBuf, EntitiesSource) {
    // Explicit non-seed `PAPR_WORDCLOUD_DIR` → load/write there.
    if std::env::var("PAPR_WORDCLOUD_DIR")
        .ok()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        && !is_shared_seed_dir(seed_dir)
    {
        return (seed_dir.join(ENTITIES_FILE), EntitiesSource::Explicit);
    }
    let cow_path = cow_dir.join(ENTITIES_FILE);
    if cow_path.is_file() {
        return (cow_path, EntitiesSource::Local);
    }
    (seed_dir.join(ENTITIES_FILE), EntitiesSource::Shared)
}

fn write_entities_file(path: &Path, doc: &EntitiesFile) -> Result<(), AppError> {
    // Refuse to overwrite the shared seed even if misconfigured.
    let seed_entities = Path::new(DEFAULT_DASHBOARD_DIR).join(ENTITIES_FILE);
    if paths_equal_relaxed(path, &seed_entities) {
        return Err(AppError::other(
            "refusing to write shared osinttools wordcloud-entities.json; use COW overlay",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::other(format!("create wordcloud dir {}: {e}", parent.display()))
        })?;
    }
    let raw = serde_json::to_string_pretty(doc)
        .map_err(|e| AppError::other(format!("serialize wordcloud entities: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{raw}\n")).map_err(|e| {
        AppError::other(format!("write wordcloud entities {}: {e}", tmp.display()))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        AppError::other(format!(
            "rename wordcloud entities {} → {}: {e}",
            tmp.display(),
            path.display()
        ))
    })?;
    Ok(())
}

fn clean_aliases(raw: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in raw {
        let t = a.trim();
        if t.is_empty() {
            continue;
        }
        let key = normalize_alias(t);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

/// `{group}.{slug}` with numeric suffix when the id already exists.
fn suggest_entity_id(group: &str, canonical: &str, existing: &[WordCloudEntity]) -> String {
    let slug = slugify_entity(canonical);
    let base = if slug.is_empty() {
        format!("{group}.entity")
    } else {
        format!("{group}.{slug}")
    };
    let taken: HashSet<&str> = existing.iter().map(|e| e.id.trim()).collect();
    if !taken.contains(base.as_str()) {
        return base;
    }
    for n in 2..10_000 {
        let candidate = format!("{base}.{n}");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
    }
    format!("{base}.{}", existing.len().saturating_add(1))
}

fn slugify_entity(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dot = true;
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dot = false;
        } else if !prev_dot {
            out.push('.');
            prev_dot = true;
        }
    }
    while out.ends_with('.') {
        out.pop();
    }
    out
}

/// Keep the previous canonical as an alias when the display name changes.
fn retain_previous_canonical_as_alias(aliases: &mut Vec<String>, old: &str, new: &str) {
    let old = old.trim();
    let new = new.trim();
    if old.is_empty() || old == new {
        return;
    }
    let old_key = normalize_alias(old);
    // Always retain when casing differs or the surface form differs, even if
    // normalize matches (e.g. ai → AI).
    let already = aliases.iter().any(|a| {
        let t = a.trim();
        t == old || normalize_alias(t) == old_key
    });
    if !already {
        // Prefer inserting at the front so the old form is easy to spot.
        aliases.insert(0, old.to_string());
    }
    // Drop aliases that exactly equal the new canonical (redundant).
    aliases.retain(|a| a.trim() != new);
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

    fn trump_fixture() -> WordCloudDict {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![
                WordCloudEntity {
                    id: "person.trump".into(),
                    canonical: "Trump".into(),
                    group: "person".into(),
                    aliases: vec![
                        "trump".into(),
                        "donald trump".into(),
                        "特朗普".into(),
                        "川普".into(),
                    ],
                },
                WordCloudEntity {
                    id: "country.china".into(),
                    canonical: "China".into(),
                    group: "country".into(),
                    aliases: vec!["china".into(), "中国".into()],
                },
            ],
        });
        dict
    }

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

    #[test]
    fn synonym_lookup_exact_alias() {
        let dict = trump_fixture();
        let g = dict.lookup_synonym_group("特朗普").expect("cn alias");
        assert_eq!(g.id, "person.trump");
        assert_eq!(g.canonical, "Trump");
        assert!(g.aliases.iter().any(|a| a == "特朗普"));
        assert!(g.aliases.iter().any(|a| a == "Trump" || a.eq_ignore_ascii_case("trump")));

        let g2 = dict.lookup_synonym_group("Trump").expect("en");
        assert_eq!(g2.id, "person.trump");
        assert!(dict.lookup_synonym_group("not-an-entity").is_none());
    }

    #[test]
    fn synonym_lookup_ambiguity_picks_smaller_id() {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![
                WordCloudEntity {
                    id: "b.second".into(),
                    canonical: "Shared".into(),
                    group: "general".into(),
                    aliases: vec!["shared".into()],
                },
                WordCloudEntity {
                    id: "a.first".into(),
                    canonical: "Also".into(),
                    group: "general".into(),
                    aliases: vec!["shared".into()],
                },
            ],
        });
        let g = dict.lookup_synonym_group("shared").expect("hit");
        assert_eq!(g.id, "a.first");
    }

    #[test]
    fn create_entity_promotes_residual_and_cows() {
        let tmp = tempfile_dir("wc-create");
        let seed = tmp.join("seed");
        let cow = tmp.join("cow");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::create_dir_all(&cow).unwrap();
        write_entities_file(
            &seed.join(ENTITIES_FILE),
            &EntitiesFile {
                version: 1,
                entities: vec![],
            },
        )
        .unwrap();

        let mut dict = WordCloudDict::empty(seed.clone());
        dict.seed_dir = seed.clone();
        dict.cow_dir = cow.clone();
        dict.reload();
        assert_eq!(dict.entities_source, EntitiesSource::Shared);

        let created = dict
            .create_entity(None, "AI", Some("general"), Some(vec!["artificial intelligence".into()]))
            .expect("create");
        assert_eq!(created.id, "general.ai");
        assert_eq!(created.canonical, "AI");
        assert!(
            created.aliases.iter().any(|a| a == "ai"),
            "lowercase residual retained: {:?}",
            created.aliases
        );
        assert!(created
            .aliases
            .iter()
            .any(|a| a == "artificial intelligence"));
        assert_eq!(dict.entities_source, EntitiesSource::Local);
        assert!(cow.join(ENTITIES_FILE).is_file());

        let (hits, _, _) = dict.match_entities("the ai boom");
        assert_eq!(hits.get("general.ai").copied().unwrap_or(0), 1);
        assert_eq!(dict.entity("general.ai").unwrap().canonical, "AI");
    }

    #[test]
    fn update_canonical_retains_old_as_alias_and_cows() {
        let tmp = tempfile_dir("wc-cow");
        let seed = tmp.join("seed");
        let cow = tmp.join("cow");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::create_dir_all(&cow).unwrap();
        let seed_entities = EntitiesFile {
            version: 1,
            entities: vec![WordCloudEntity {
                id: "tech.ai".into(),
                canonical: "ai".into(),
                group: "general".into(),
                aliases: vec!["artificial intelligence".into()],
            }],
        };
        write_entities_file(&seed.join(ENTITIES_FILE), &seed_entities).unwrap();

        let mut dict = WordCloudDict::empty(seed.clone());
        dict.seed_dir = seed.clone();
        dict.cow_dir = cow.clone();
        dict.reload();
        assert_eq!(dict.entities_source, EntitiesSource::Shared);
        assert!(!cow.join(ENTITIES_FILE).exists());

        let updated = dict
            .update_entity("tech.ai", Some("AI"), None)
            .expect("update");
        assert_eq!(updated.canonical, "AI");
        assert!(
            updated.aliases.iter().any(|a| a == "ai"),
            "old canonical must remain an alias: {:?}",
            updated.aliases
        );
        assert!(updated
            .aliases
            .iter()
            .any(|a| a == "artificial intelligence"));
        assert_eq!(dict.entities_source, EntitiesSource::Local);
        assert!(cow.join(ENTITIES_FILE).is_file());
        // Shared seed untouched.
        let seed_doc: EntitiesFile =
            serde_json::from_str(&std::fs::read_to_string(seed.join(ENTITIES_FILE)).unwrap())
                .unwrap();
        assert_eq!(seed_doc.entities[0].canonical, "ai");
        assert_eq!(seed_doc.version, 1);

        // Matching still hits lowercase body text; display canonical is AI.
        let (hits, _, _) = dict.match_entities("the ai boom");
        assert_eq!(hits.get("tech.ai").copied().unwrap_or(0), 1);
        assert_eq!(dict.entity("tech.ai").unwrap().canonical, "AI");

        // Synonym lookup via old surface.
        let g = dict.lookup_synonym_group("ai").expect("syn");
        assert_eq!(g.canonical, "AI");
    }

    #[test]
    fn update_refuses_shared_seed_path() {
        let err = write_entities_file(
            Path::new(DEFAULT_DASHBOARD_DIR).join(ENTITIES_FILE).as_path(),
            &EntitiesFile::default(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refusing") || msg.contains("COW"),
            "unexpected: {msg}"
        );
    }

    fn tempfile_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "papr-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
