use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const VERSION: u32 = 1;
/// The evidence exclusion policy new archives are frozen under. A stored archive is validated
/// under the policy it records, so a rule change is a new version, never an edit of an old one.
pub const POLICY_VERSION: u32 = 2;
/// Policies whose stored archives still validate.
const STORED_POLICY_VERSIONS: [u32; 2] = [1, 2];
pub const MAX_FILE_BYTES: u64 = 1_000_000;
pub const MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
pub const COMPACT_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Frozen evidence contract or scope is invalid")]
    Contract,
    #[error("Frozen evidence integrity check failed")]
    Integrity,
    #[error("Frozen evidence exceeds its approved budget")]
    Budget,
    #[error("Frozen evidence {resource} exceeds its approved budget: {actual} > {limit} {unit}")]
    Limit {
        resource: &'static str,
        actual: u64,
        limit: u64,
        unit: &'static str,
    },
    #[error("Frozen evidence path is invalid")]
    Path,
    #[error("Evidence revision is outside this snapshot")]
    Revision,
    #[error("Evidence path is absent from this revision")]
    Absent,
    #[error("Evidence path is excluded by policy: {0:?}")]
    Excluded(Exclusion),
    #[error("Frozen evidence preparation failed")]
    Preparation,
    #[error("Review archives still use the SQLite index; run `ocr-server import-sqlite` before starting the server")]
    UnimportedIndex,
    #[error("cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scope {
    pub host: String,
    pub repository: String,
    pub head_sha: String,
    pub base_sha: String,
    pub merge_base_sha: String,
    pub prior_sha: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourceEntry {
    pub path: String,
    pub revision: String,
    pub mode: String,
    pub git_blob_id: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Exclusion {
    NonRegular,
    PrivateConfiguration,
    PrivatePath,
    BinaryPath,
    Generated,
    Oversized,
    Binary,
    NonUtf8,
    /// Left out because the archive's blob budget was full; see `Archive::build_within`.
    Budget,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Entry {
    pub source: SourceEntry,
    pub content_hash: Option<String>,
    pub exclusion: Option<Exclusion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Blob {
    pub hash: String,
    pub offset: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub policy_version: u32,
    pub scope: Scope,
    pub entries: Vec<Entry>,
    pub blobs: Vec<Blob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Commit,
    Branch,
    Tag,
}

impl std::fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReferenceKind::Commit => write!(f, "commit"),
            ReferenceKind::Branch => write!(f, "branch"),
            ReferenceKind::Tag => write!(f, "tag"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DependencyProvenance {
    pub url: String,
    pub repository: String,
    pub reference: String,
    pub reference_kind: ReferenceKind,
    pub path: String,
    pub commit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Dependency {
    pub repository: String,
    pub revision: String,
    pub entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<DependencyProvenance>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Descriptor {
    pub version: u32,
    pub id: String,
    pub manifest_bytes: u64,
    pub blob_bytes: u64,
    pub entry_count: usize,
    pub expanded_bytes: u64,
}

#[derive(Debug)]
pub struct Archive {
    manifest: Manifest,
    data: Vec<u8>,
    descriptor: Descriptor,
    compact: bool,
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A manifest entry's approximate encoded size beside its path.
const ENTRY_OVERHEAD: u64 = 200;

/// Which `sources` (sorted) an archive keeps within `manifest_room` and `blob_room`, priority
/// paths first: the kept flags, and the kept entries whose content no longer fits.
fn plan_budget(
    sources: &[SourceEntry],
    priority: &BTreeSet<String>,
    manifest_room: u64,
    blob_room: u64,
) -> Result<(Vec<bool>, BTreeSet<(String, String)>), Error> {
    let order = (0..sources.len())
        .filter(|&index| priority.contains(&sources[index].path))
        .chain((0..sources.len()).filter(|&index| !priority.contains(&sources[index].path)));
    let mut kept = vec![false; sources.len()];
    let mut excluded = BTreeSet::new();
    let (mut manifest, mut blobs, mut entries) = (0u64, 0u64, 0usize);
    let mut known = BTreeSet::new();
    for index in order {
        let source = &sources[index];
        let required = priority.contains(&source.path);
        let size = source.path.len() as u64 + ENTRY_OVERHEAD;
        if !required && (manifest + size > manifest_room || entries >= MAX_ENTRIES) {
            continue;
        }
        manifest += size;
        entries += 1;
        kept[index] = true;
        if source.exclusion()?.is_some() || known.contains(&source.git_blob_id) {
            continue;
        }
        if !required && blobs + source.bytes > blob_room {
            excluded.insert((source.revision.clone(), source.path.clone()));
            continue;
        }
        blobs += source.bytes;
        known.insert(source.git_blob_id.clone());
    }
    Ok((kept, excluded))
}

pub fn validate_limits(blob: u64, manifest: u64, entries: usize) -> Result<(), Error> {
    for (resource, actual, limit, unit) in [
        ("blobs", blob, MAX_BLOB_BYTES, "bytes"),
        ("manifest", manifest, MAX_MANIFEST_BYTES, "bytes"),
        ("manifest entries", entries as u64, MAX_ENTRIES as u64, "entries"),
    ] {
        if actual > limit {
            return Err(Error::Limit {
                resource,
                actual,
                limit,
                unit,
            });
        }
    }
    Ok(())
}

fn oid(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn safe_path(path: &str) -> Result<(), Error> {
    if path.is_empty()
        || path.len() > 512
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(Error::Path);
    }
    Ok(())
}

impl Scope {
    pub fn revisions(&self) -> BTreeSet<&str> {
        [&self.head_sha, &self.merge_base_sha]
            .into_iter()
            .chain(self.prior_sha.iter())
            .map(String::as_str)
            .collect()
    }

    pub fn validate(&self) -> Result<(), Error> {
        let names = self.repository.split('/').collect::<Vec<_>>();
        // GitLab projects may nest under subgroups (`acme/team/api`).
        if self.host.len() > 253
            || !crate::gitlab::is_valid_gitlab_host(&self.host)
            || self.repository.len() > 256
            || !(2..=20).contains(&names.len())
            || names.iter().any(|name| {
                name.is_empty()
                    || matches!(*name, "." | "..")
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
        {
            return Err(Error::Contract);
        }
        let length = self.head_sha.len();
        if !matches!(length, 40 | 64)
            || self
                .revisions()
                .into_iter()
                .chain([self.base_sha.as_str()])
                .any(|sha| !oid(sha, length))
        {
            return Err(Error::Revision);
        }
        Ok(())
    }
}

impl SourceEntry {
    fn validate(&self, scope: &Scope) -> Result<(), Error> {
        safe_path(&self.path)?;
        if !scope.revisions().contains(self.revision.as_str()) {
            return Err(Error::Revision);
        }
        if !oid(&self.git_blob_id, self.revision.len())
            || !matches!(self.mode.as_str(), "100644" | "100755" | "120000" | "160000")
        {
            return Err(Error::Contract);
        }
        Ok(())
    }

    /// Why the current policy leaves this source out of an archive, if it does.
    pub fn exclusion(&self) -> Result<Option<Exclusion>, Error> {
        self.exclusion_for(POLICY_VERSION)
    }

    fn exclusion_for(&self, policy: u32) -> Result<Option<Exclusion>, Error> {
        safe_path(&self.path)?;
        if !matches!(self.mode.as_str(), "100644" | "100755") {
            return Ok(Some(Exclusion::NonRegular));
        }
        if let Some(reason) = path_exclusion_for(&self.path, policy)? {
            return Ok(Some(reason));
        }
        Ok((self.bytes > MAX_FILE_BYTES).then_some(Exclusion::Oversized))
    }
}

/// Paths inside an AI tool's configuration folder. They are reviewed from the frozen archive only,
/// never copied into a runner's workspace, where an AI CLI could load them as its own
/// configuration (the runner refuses such stage files).
pub(crate) fn in_agent_folder(path: &str) -> bool {
    path.to_ascii_lowercase().split('/').any(|part| {
        matches!(
            part,
            ".devin" | ".agents" | ".claude" | ".gemini" | ".cursor" | ".windsurf" | ".opencode" | ".zed"
        )
    })
}

/// Why the current policy leaves this path out of an archive, if it does.
pub(crate) fn path_exclusion(path: &str) -> Result<Option<Exclusion>, Error> {
    path_exclusion_for(path, POLICY_VERSION)
}

/// Directories policy 2 adds to policy 1's generated ones: vendored dependencies and generated code.
const POLICY_2_GENERATED_DIRS: &[&str] = &["vendor", "generated"];
/// File name endings policy 2 adds: minified styles, source maps, protobuf and Go generated code.
const POLICY_2_GENERATED_SUFFIXES: &[&str] = &[
    "generated.ts",
    "generated.js",
    ".min.css",
    ".js.map",
    ".css.map",
    ".pb.go",
    ".pb.h",
    ".pb.cc",
    "_pb2.py",
    "_pb2_grpc.py",
    "_gen.go",
    ".lockb",
];
/// Lockfile names policy 2 adds to policy 1's `.lock` and `lock.yaml` endings.
const POLICY_2_LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "go.sum",
    "package.resolved",
    "packages.lock.json",
];

fn path_exclusion_for(path: &str, policy: u32) -> Result<Option<Exclusion>, Error> {
    safe_path(path)?;
    let path = path.to_ascii_lowercase();
    let parts = path.split('/').collect::<Vec<_>>();
    let name = parts.last().ok_or(Error::Path)?;
    if parts.iter().any(|part| matches!(*part, ".git" | ".ssh" | ".gnupg")) {
        return Ok(Some(Exclusion::PrivateConfiguration));
    }
    // Committed agent instructions, skills and scripts are reviewable code; their settings, MCP
    // servers, sign-ins and session state are not.
    if *name == ".mcp.json"
        || (in_agent_folder(&path)
            && (name.contains(".local.")
                || name.contains("credentials")
                || matches!(
                    *name,
                    "settings.json" | "mcp.json" | "auth.json" | "oauth_creds.json" | "history.jsonl"
                )
                || parts
                    .iter()
                    .any(|part| matches!(*part, "projects" | "sessions" | "todos" | "shell-snapshots" | "statsig"))))
    {
        return Ok(Some(Exclusion::PrivateConfiguration));
    }
    if name.starts_with(".env")
        || matches!(
            *name,
            "credentials" | "credentials.json" | "credentials.toml" | "hosts.yml" | "id_rsa" | "id_ed25519"
        )
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"]
            .iter()
            .any(|ext| name.ends_with(ext))
    {
        return Ok(Some(Exclusion::PrivatePath));
    }
    if [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".ttf", ".otf", ".woff", ".woff2", ".mp3",
        ".mp4", ".wasm", ".bin", ".so", ".dylib", ".dll", ".exe",
    ]
    .iter()
    .any(|ext| path.ends_with(ext))
    {
        return Ok(Some(Exclusion::BinaryPath));
    }
    if parts[..parts.len() - 1].iter().any(|part| {
        matches!(
            *part,
            "node_modules" | "dist" | "build" | ".next" | "coverage" | "__generated__"
        )
    }) || path.contains(".generated.")
        || [".min.js", ".lock", "lock.yaml"].iter().any(|ext| path.ends_with(ext))
    {
        return Ok(Some(Exclusion::Generated));
    }
    // Policy 2 only adds generated paths after every policy 1 rule, so a private or binary path
    // keeps its reason under both.
    if policy >= 2
        && (parts[..parts.len() - 1]
            .iter()
            .any(|part| POLICY_2_GENERATED_DIRS.contains(part))
            || name.starts_with("zz_generated")
            || POLICY_2_GENERATED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
            || POLICY_2_LOCKFILES.contains(name))
    {
        return Ok(Some(Exclusion::Generated));
    }
    Ok(None)
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION || !oid(&self.id, 64) {
            return Err(Error::Contract);
        }
        validate_limits(self.blob_bytes, self.manifest_bytes, self.entry_count)
    }
}

impl Manifest {
    fn validate(&self, data: &[u8]) -> Result<u64, Error> {
        if self.version != VERSION || !STORED_POLICY_VERSIONS.contains(&self.policy_version) {
            return Err(Error::Contract);
        }
        self.validate_sources()?;
        validate_limits(data.len() as u64, 0, self.entry_count())?;
        if self.blobs.len() > self.entry_count() {
            return Err(Error::Integrity);
        }
        let mut offset = 0u64;
        let mut blobs = BTreeMap::new();
        for blob in &self.blobs {
            if !oid(&blob.hash, 64)
                || blob.offset != offset
                || blob.bytes > MAX_FILE_BYTES
                || blobs.insert(blob.hash.as_str(), blob).is_some()
            {
                return Err(Error::Integrity);
            }
            let end = offset
                .checked_add(blob.bytes)
                .filter(|end| *end <= data.len() as u64)
                .ok_or(Error::Integrity)?;
            let bytes = &data[offset as usize..end as usize];
            if hash(bytes) != blob.hash || bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
                return Err(Error::Integrity);
            }
            offset = end;
        }
        if offset != data.len() as u64 {
            return Err(Error::Integrity);
        }
        let mut used = BTreeSet::new();
        let mut expanded = 0u64;
        for entry in self.all_entries() {
            match (
                entry.source.exclusion_for(self.policy_version)?,
                entry.exclusion,
                entry.content_hash.as_deref(),
            ) {
                (Some(expected), Some(actual), None) if expected == actual => {}
                (None, Some(Exclusion::Binary | Exclusion::NonUtf8), None) if entry.source.bytes > 0 => {}
                (None, Some(Exclusion::Budget), None) => {}
                (None, None, Some(digest)) => {
                    let blob = blobs.get(digest).ok_or(Error::Integrity)?;
                    if blob.bytes != entry.source.bytes {
                        return Err(Error::Integrity);
                    }
                    used.insert(digest);
                    expanded = expanded.checked_add(blob.bytes).ok_or(Error::Budget)?;
                }
                _ => return Err(Error::Contract),
            }
        }
        if used.len() != blobs.len() {
            return Err(Error::Integrity);
        }
        Ok(expanded)
    }
}

impl Archive {
    pub fn build(
        scope: Scope,
        sources: Vec<SourceEntry>,
        cancel: &AtomicBool,
        load: impl FnMut(&SourceEntry) -> Result<Vec<u8>, Error>,
    ) -> Result<Self, Error> {
        Self::build_within(scope, sources, &BTreeSet::new(), cancel, load)
    }

    /// Builds the archive within its budgets even for a repository larger than them: the
    /// `priority` paths (what the review is about) go first, then the rest in path order. Past the
    /// blob budget an entry stays listed as excluded; past the manifest budget it is left out, as
    /// if absent. Only the priority paths alone can still exceed a budget.
    pub fn build_within(
        scope: Scope,
        mut sources: Vec<SourceEntry>,
        priority: &BTreeSet<String>,
        cancel: &AtomicBool,
        mut load: impl FnMut(&SourceEntry) -> Result<Vec<u8>, Error>,
    ) -> Result<Self, Error> {
        scope.validate()?;
        sources.sort_by(|a, b| (&a.revision, &a.path).cmp(&(&b.revision, &b.path)));
        for (index, source) in sources.iter().enumerate() {
            source.validate(&scope)?;
            if index > 0 && (&sources[index - 1].revision, &sources[index - 1].path) == (&source.revision, &source.path)
            {
                return Err(Error::Contract);
            }
        }
        let mut manifest_room = MAX_MANIFEST_BYTES;
        loop {
            let (kept, excluded) = plan_budget(&sources, priority, manifest_room, MAX_BLOB_BYTES)?;
            let kept = sources
                .iter()
                .zip(kept)
                .filter(|(_, kept)| *kept)
                .map(|(source, _)| source.clone())
                .collect();
            match Self::build_planned(scope.clone(), kept, &excluded, cancel, &mut load) {
                // The manifest estimate ran short: plan again for proportionally less room.
                Err(Error::Limit {
                    resource: "manifest",
                    actual,
                    ..
                }) if manifest_room > MAX_MANIFEST_BYTES / 4 => {
                    manifest_room = manifest_room * MAX_MANIFEST_BYTES / actual * 19 / 20;
                }
                result => return result,
            }
        }
    }

    fn build_planned(
        scope: Scope,
        sources: Vec<SourceEntry>,
        excluded: &BTreeSet<(String, String)>,
        cancel: &AtomicBool,
        mut load: impl FnMut(&SourceEntry) -> Result<Vec<u8>, Error>,
    ) -> Result<Self, Error> {
        validate_limits(0, 0, sources.len())?;
        let mut manifest = Manifest {
            version: VERSION,
            policy_version: POLICY_VERSION,
            scope,
            entries: Vec::new(),
            blobs: Vec::new(),
            dependencies: Vec::new(),
        };
        let mut data = Vec::new();
        let mut known = BTreeSet::new();
        let mut expanded = 0u64;
        for source in sources {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let budget = excluded.contains(&(source.revision.clone(), source.path.clone()));
            let mut entry = Entry {
                exclusion: source.exclusion()?.or(budget.then_some(Exclusion::Budget)),
                source,
                content_hash: None,
            };
            if entry.exclusion.is_none() {
                let bytes = load(&entry.source)?;
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                if bytes.len() as u64 != entry.source.bytes {
                    return Err(Error::Integrity);
                }
                entry.exclusion = if bytes.contains(&0) {
                    Some(Exclusion::Binary)
                } else if std::str::from_utf8(&bytes).is_err() {
                    Some(Exclusion::NonUtf8)
                } else {
                    None
                };
                if entry.exclusion.is_none() {
                    expanded = expanded.checked_add(entry.source.bytes).ok_or(Error::Budget)?;
                    let digest = hash(&bytes);
                    let additional = if known.contains(&digest) { 0 } else { entry.source.bytes };
                    validate_limits(data.len() as u64 + additional, 0, manifest.entries.len() + 1)?;
                    if known.insert(digest.clone()) {
                        manifest.blobs.push(Blob {
                            hash: digest.clone(),
                            offset: data.len() as u64,
                            bytes: entry.source.bytes,
                        });
                        data.extend_from_slice(&bytes);
                    }
                    entry.content_hash = Some(digest);
                }
            }
            manifest.entries.push(entry);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut raw = manifest::encode(&manifest, false)?;
        if raw.len() as u64 > COMPACT_MANIFEST_BYTES {
            raw = manifest::encode(&manifest, true)?;
        }
        let descriptor = Descriptor {
            version: VERSION,
            id: hash(&raw),
            manifest_bytes: raw.len() as u64,
            blob_bytes: data.len() as u64,
            entry_count: manifest.entries.len(),
            expanded_bytes: expanded,
        };
        Self::decode(&descriptor, &raw, data)
    }

    pub fn decode(descriptor: &Descriptor, raw: &[u8], data: Vec<u8>) -> Result<Self, Error> {
        descriptor.validate()?;
        if raw.len() as u64 != descriptor.manifest_bytes
            || data.len() as u64 != descriptor.blob_bytes
            || hash(raw) != descriptor.id
        {
            return Err(Error::Integrity);
        }
        let (manifest, compact) = manifest::decode(raw)?;
        let expanded = manifest.validate(&data)?;
        if manifest.entry_count() != descriptor.entry_count || expanded != descriptor.expanded_bytes {
            return Err(Error::Integrity);
        }
        Ok(Self {
            manifest,
            data,
            descriptor: descriptor.clone(),
            compact,
        })
    }

    pub fn resolve(&self, revision: &str, path: &str) -> Result<&str, Error> {
        safe_path(path)?;
        if !self.manifest.scope.revisions().contains(revision) {
            return Err(Error::Revision);
        }
        let index = self
            .manifest
            .entries
            .binary_search_by(|entry| {
                (entry.source.revision.as_str(), entry.source.path.as_str()).cmp(&(revision, path))
            })
            .map_err(|_| Error::Absent)?;
        let entry = &self.manifest.entries[index];
        if let Some(reason) = entry.exclusion {
            return Err(Error::Excluded(reason));
        }
        let blob = self
            .manifest
            .blobs
            .iter()
            .find(|blob| Some(&blob.hash) == entry.content_hash.as_ref())
            .ok_or(Error::Integrity)?;
        std::str::from_utf8(&self.data[blob.offset as usize..(blob.offset + blob.bytes) as usize])
            .map_err(|_| Error::Integrity)
    }

    pub fn write_new(&self, path: &Path) -> Result<(), Error> {
        let raw = self.manifest_bytes()?;
        let mut file = open_archive(path, true)?;
        file.write_all(b"OCRARCH1")
            .and_then(|_| file.write_all(&(raw.len() as u32).to_le_bytes()))
            .and_then(|_| file.write_all(&raw))
            .and_then(|_| file.write_all(&self.data))
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Preparation)
    }

    pub fn read(path: &Path, descriptor: &Descriptor) -> Result<Self, Error> {
        descriptor.validate()?;
        let mut file = open_archive(path, false)?;
        let metadata = file.metadata().map_err(|_| Error::Preparation)?;
        if !metadata.is_file() || metadata.len() != 12 + descriptor.manifest_bytes + descriptor.blob_bytes {
            return Err(Error::Integrity);
        }
        let mut header = [0; 12];
        file.read_exact(&mut header).map_err(|_| Error::Integrity)?;
        if &header[..8] != b"OCRARCH1"
            || u32::from_le_bytes(header[8..].try_into().map_err(|_| Error::Integrity)?) as u64
                != descriptor.manifest_bytes
        {
            return Err(Error::Integrity);
        }
        let mut raw = vec![0; descriptor.manifest_bytes as usize];
        file.read_exact(&mut raw).map_err(|_| Error::Integrity)?;
        let mut data = Vec::new();
        file.take(descriptor.blob_bytes + 1)
            .read_to_end(&mut data)
            .map_err(|_| Error::Integrity)?;
        Self::decode(descriptor, &raw, data)
    }

    pub fn transfer_blobs(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.manifest.blobs.iter().map(|blob| {
            (
                blob.hash.as_str(),
                &self.data[blob.offset as usize..(blob.offset + blob.bytes) as usize],
            )
        })
    }

    pub fn materialize_new(&self, root: &Path, cancel: &AtomicBool) -> Result<(), Error> {
        use std::fs;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let parent = root.parent().ok_or(Error::Path)?;
        if parent.canonicalize().map_err(|_| Error::Preparation)? != parent {
            return Err(Error::Path);
        }
        fs::create_dir(root).map_err(|_| Error::Preparation)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Preparation)?;
        }
        let mut written = BTreeMap::new();
        for entry in self.entries().iter().filter(|entry| entry.exclusion.is_none()) {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let source = &entry.source;
            self.write_source(
                &root.join("trees").join(&source.revision).join(&source.path),
                entry,
                || self.resolve(&source.revision, &source.path),
                &mut written,
            )?;
        }
        self.materialize_dependencies(root, cancel, &mut written)?;
        let mut manifest = open_archive(&root.join("manifest.json"), true)?;
        manifest
            .write_all(&self.manifest_bytes()?)
            .map_err(|_| Error::Preparation)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            manifest
                .set_permissions(fs::Permissions::from_mode(0o444))
                .map_err(|_| Error::Preparation)?;
            let mut directories = vec![root.to_path_buf()];
            while let Some(directory) = directories.pop() {
                for entry in fs::read_dir(&directory).map_err(|_| Error::Preparation)? {
                    let entry = entry.map_err(|_| Error::Preparation)?;
                    if fs::symlink_metadata(entry.path())
                        .map_err(|_| Error::Preparation)?
                        .is_dir()
                    {
                        directories.push(entry.path());
                    }
                }
                fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).map_err(|_| Error::Preparation)?;
            }
        }
        Ok(())
    }

    pub(super) fn write_source<'a>(
        &'a self,
        target: &Path,
        entry: &Entry,
        content: impl FnOnce() -> Result<&'a str, Error>,
        written: &mut BTreeMap<String, std::path::PathBuf>,
    ) -> Result<(), Error> {
        use std::fs;
        fs::create_dir_all(target.parent().ok_or(Error::Path)?).map_err(|_| Error::Preparation)?;
        let digest = entry.content_hash.as_ref().ok_or(Error::Integrity)?;
        if let Some(first) = written.get(digest) {
            return fs::hard_link(first, target).map_err(|_| Error::Preparation);
        }
        let mut file = open_archive(target, true)?;
        file.write_all(content()?.as_bytes()).map_err(|_| Error::Preparation)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o444))
                .map_err(|_| Error::Preparation)?;
        }
        written.insert(digest.clone(), target.to_path_buf());
        Ok(())
    }

    pub fn validate_assigned(&self, files: &[String], diff: &str, before: &str) -> Result<(), Error> {
        if !self.scope().revisions().contains(before) {
            return Err(Error::Revision);
        }
        let mut old = None;
        let mut removed = BTreeSet::new();
        for line in diff.lines() {
            if line.starts_with("diff --git ") {
                old = None;
            }
            if let Some(raw) = line.strip_prefix("--- ") {
                old = decoded_git_path(raw).and_then(|path| path.strip_prefix("a/").map(str::to_string));
            }
            if let Some(raw) = line.strip_prefix("rename from ") {
                old = decoded_git_path(raw);
            }
            if let Some(raw) = line.strip_prefix("rename to ") {
                if let Some(path) = old.take().filter(|path| files.contains(path)) {
                    let destination = decoded_git_path(raw).ok_or(Error::Path)?;
                    if !self
                        .entries()
                        .iter()
                        .any(|entry| entry.source.revision == self.scope().head_sha && entry.source.path == destination)
                    {
                        return Err(Error::Absent);
                    }
                    removed.insert(path);
                }
            }
            if line == "+++ /dev/null" {
                if let Some(path) = old.take() {
                    removed.insert(path);
                }
            }
        }
        for path in files {
            match self.resolve(&self.scope().head_sha, path) {
                Ok(_) => {}
                Err(Error::Absent) if removed.contains(path.as_str()) => {
                    self.resolve(before, path)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn protocol(&self) -> String {
        let dependencies = self.dependency_protocol();
        let manifest_layout = if self.compact {
            " Compact manifest entryGroups preserve entry.source metadata; each entry also exists unchanged at every SHA in its additionalRevisions list."
        } else {
            ""
        };
        format!("\nFrozen evidence contract v2 (replaces the legacy output/context-retrieval schema): all allowed code is already available at /evidence/snapshot/trees/<revision>/<repository-path>. Search /evidence/snapshot/manifest.json for paths, revisions and exclusions, then read/search source progressively.{manifest_layout} The initial context is only a selection; consult this archive for callers, implementations, guards and transitive dependencies during this same execution. Never exit to request another model attempt. Use only the permitted snapshot-reading commands. No project commands, network, MCP, checkout or unprepared repositories. Include contractVersion:2 and snapshotId:\"{}\" in the report and findingDecisions objects. Every code evidence/counterEvidence reference must include revision as an exact supplied SHA, in addition to path, line and quote. Current head: {}. Permitted source revisions: {}. PR.md references have no revision. Convention references additionally use source:\"convention\", a repository-relative path and revision {}. Cite exact supplied bytes; excluded/absent essential evidence remains missingEvidence, never clean. Keep requestedFiles empty. reviewedFiles contains exactly the assigned paths, not supporting code. PR_DIFF.patch is the analysis scope: when incremental, review only its new changes and reconcile pending findings, not the entire PR again. If PUBLICATION_DIFF.patch is present, it is only the GitHub anchor reference for these assigned paths, not additional analysis scope. Inline findings must use added RIGHT or removed LEFT lines from PUBLICATION_DIFF.patch when supplied, otherwise PR_DIFF.patch. LEFT publication coordinates refer to the merge-base, never the previously reviewed head; prior-head evidence still cites its own revision and line. Never invent an anchor or silently discard an unsupported concern. Read PR.md and the active skill from the initial context.\n{dependencies}", self.descriptor.id, self.scope().head_sha, self.scope().revisions().into_iter().collect::<Vec<_>>().join(", "), self.scope().base_sha)
    }

    pub fn scope(&self) -> &Scope {
        &self.manifest.scope
    }
    pub fn entries(&self) -> &[Entry] {
        &self.manifest.entries
    }
    pub fn descriptor(&self) -> Descriptor {
        self.descriptor.clone()
    }
    pub fn blobs(&self) -> &[u8] {
        &self.data
    }
    pub fn manifest_bytes(&self) -> Result<Vec<u8>, Error> {
        manifest::encode(&self.manifest, self.compact)
    }
}

pub(crate) fn decoded_git_path(raw: &str) -> Option<String> {
    if !raw.starts_with('"') {
        return Some(raw.to_string());
    }
    let raw = raw.strip_prefix('"')?.strip_suffix('"')?.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let byte = raw[index];
        index += 1;
        if byte != b'\\' {
            output.push(byte);
            continue;
        }
        let byte = *raw.get(index)?;
        index += 1;
        match byte {
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    if let Some(next @ b'0'..=b'7') = raw.get(index).copied() {
                        value = value * 8 + u16::from(next - b'0');
                        index += 1;
                    } else {
                        break;
                    }
                }
                output.push(u8::try_from(value).ok()?);
            }
            b'\\' | b'"' => output.push(byte),
            b't' => output.push(b'\t'),
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b'a' => output.push(7),
            b'b' => output.push(8),
            b'v' => output.push(11),
            b'f' => output.push(12),
            _ => return None,
        }
    }
    String::from_utf8(output).ok()
}

pub(crate) fn read_prepared_text(root: &Path, path: &str) -> Result<String, Error> {
    crate::sandbox::safe_path(path).map_err(|_| Error::Path)?;
    let root = root.canonicalize().map_err(|_| Error::Preparation)?;
    let mut bytes = Vec::new();
    let file = open_archive(&root.join(path), false)?;
    if !file.metadata().map_err(|_| Error::Preparation)?.is_file() {
        return Err(Error::Path);
    }
    file.take(crate::review::policy::MAX_EVIDENCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Preparation)?;
    if bytes.len() > crate::review::policy::MAX_EVIDENCE_BYTES {
        return Err(Error::Budget);
    }
    String::from_utf8(bytes).map_err(|_| Error::Integrity)
}

#[cfg(unix)]
fn open_archive(path: &Path, create: bool) -> Result<File, Error> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Error::Path);
    }
    let names = components
        .map(|part| match part {
            Component::Normal(name) => CString::new(name.as_bytes()).map_err(|_| Error::Path),
            _ => Err(Error::Path),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (name, parents) = names.split_last().ok_or(Error::Path)?;
    let mut directory = File::open("/").map_err(|_| Error::Preparation)?;
    for name in parents {
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Error::Preparation);
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let flags = if create {
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
    } else {
        libc::O_RDONLY | libc::O_NONBLOCK
    };
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o444 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(Error::Preparation);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn open_archive(_path: &Path, _create: bool) -> Result<File, Error> {
    Err(Error::Preparation)
}

mod dependencies;
mod git;
mod manifest;
pub mod purge;
pub mod retention;
pub use git::{capture, capture_prioritized, capture_selected, hydrate};
mod transfer;
pub use transfer::{transfer_manifest, validate_blob, Incoming};
mod store;
pub use store::{ArchiveStore, STORE_QUOTA};

/// Where frozen review archives live under the server's data directory. The server and the
/// `archive-retention` command both resolve it here, so they always agree.
pub fn archive_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("review-archives")
}
mod stage;
pub use stage::Stage;

#[cfg(test)]
mod tests;
