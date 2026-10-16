use serde::{Deserialize, Deserializer};
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

const MAX_METADATA_SIZE: u64 = 1024 * 1024;
const MAX_ARCHIVE_SIZE: u64 = 128 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricMetadata {
    #[serde(default, deserialize_with = "deserialize_present")]
    schema_version: Option<u32>,
    pub id: String,
    pub version: String,
    #[serde(default, deserialize_with = "deserialize_present")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    pub environment: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    depends: Option<BTreeMap<String, VersionRequirement>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    recommends: Option<BTreeMap<String, VersionRequirement>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    suggests: Option<BTreeMap<String, VersionRequirement>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    breaks: Option<BTreeMap<String, VersionRequirement>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    conflicts: Option<BTreeMap<String, VersionRequirement>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    pub mixins: Option<Vec<MixinEntry>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    entrypoints: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default, deserialize_with = "deserialize_present")]
    pub access_widener: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present")]
    pub jars: Option<Vec<NestedJar>>,
}

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl FabricMetadata {
    pub fn entrypoint_count(&self) -> usize {
        self.entrypoints
            .as_ref()
            .map(|entrypoints| {
                entrypoints
                    .values()
                    .map(|value| match value {
                        serde_json::Value::Array(entries) => entries.len(),
                        _ => 1,
                    })
                    .sum()
            })
            .unwrap_or(0)
    }

    fn validate(&self, source: &Path) -> Vec<String> {
        let mut errors = Vec::new();
        let file = source.display();

        if self.schema_version != Some(1) {
            errors.push(format!(
                "{file}: field 'schemaVersion' must be 1 (found {:?})",
                self.schema_version
            ));
        }
        if !valid_mod_id(&self.id) {
            errors.push(format!(
                "{file}: field 'id' has invalid mod ID '{}'",
                self.id
            ));
        }
        if self.version.is_empty() || self.version.chars().any(char::is_control) {
            errors.push(format!(
                "{file}: field 'version' must be non-empty and contain no control characters"
            ));
        }
        if let Some(environment) = &self.environment {
            if !valid_environment(environment) {
                errors.push(format!(
                    "{file}: field 'environment' must be '*', 'client', or 'server' (found '{environment}')"
                ));
            }
        }

        for (field, dependencies) in [
            ("depends", self.depends.as_ref()),
            ("recommends", self.recommends.as_ref()),
            ("suggests", self.suggests.as_ref()),
            ("breaks", self.breaks.as_ref()),
            ("conflicts", self.conflicts.as_ref()),
        ] {
            if let Some(dependencies) = dependencies {
                for (dependency_id, requirement) in dependencies {
                    if !valid_mod_id(dependency_id) {
                        errors.push(format!(
                            "{file}: field '{field}' contains invalid mod ID '{dependency_id}'"
                        ));
                    }
                    if let Err(error) = requirement.validate() {
                        errors.push(format!(
                            "{file}: invalid version requirement for '{dependency_id}' in '{field}': {error}"
                        ));
                    }
                }
            }
        }

        if let Some(entrypoints) = &self.entrypoints {
            for (key, entrypoint) in entrypoints {
                let valid_value = match entrypoint {
                    serde_json::Value::Array(values) => {
                        values.iter().all(valid_entrypoint_declaration)
                    }
                    _ => valid_entrypoint_declaration(entrypoint),
                };
                if !valid_value {
                    errors.push(format!(
                        "{file}: field 'entrypoints.{key}' must be a string or an array of strings"
                    ));
                }
            }
        }

        fn valid_entrypoint_declaration(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::String(_) => true,
                serde_json::Value::Object(entrypoint) => {
                    entrypoint
                        .get("value")
                        .is_some_and(serde_json::Value::is_string)
                        && entrypoint
                            .get("adapter")
                            .map_or(true, serde_json::Value::is_string)
                }
                _ => false,
            }
        }

        if let Some(mixins) = &self.mixins {
            for mixin in mixins {
                if !valid_archive_path(mixin.config()) {
                    errors.push(format!(
                        "{file}: invalid Mixin config path '{}'",
                        mixin.config()
                    ));
                }
                if !valid_environment(mixin.environment()) {
                    errors.push(format!(
                        "{file}: invalid environment '{}' for Mixin config '{}'",
                        mixin.environment(),
                        mixin.config()
                    ));
                }
            }
        }

        if let Some(jars) = &self.jars {
            for jar in jars {
                if !valid_archive_path(&jar.file) {
                    errors.push(format!("{file}: invalid nested JAR path '{}'", jar.file));
                }
            }
        }
        errors
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VersionRequirement {
    One(String),
    Any(Vec<String>),
}

impl VersionRequirement {
    fn alternatives(&self) -> Vec<&str> {
        match self {
            Self::One(value) => vec![value],
            Self::Any(values) => values.iter().map(String::as_str).collect(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let alternatives = self.alternatives();
        if alternatives.is_empty() {
            return Err("at least one predicate is required".to_string());
        }
        for predicate in alternatives {
            parse_predicate(predicate)?;
        }
        Ok(())
    }

    fn matches(&self, installed_version: &str) -> Result<bool, String> {
        for predicate in self.alternatives() {
            if parse_predicate(predicate)?
                .iter()
                .all(|term| term.matches(installed_version))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl std::fmt::Display for VersionRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::One(value) => write!(f, "{value}"),
            Self::Any(values) => write!(f, "{}", values.join(" OR ")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MixinEntry {
    Path(String),
    Config {
        config: String,
        #[serde(default, deserialize_with = "deserialize_present")]
        environment: Option<String>,
    },
}

impl MixinEntry {
    pub fn config(&self) -> &str {
        match self {
            Self::Path(path) => path,
            Self::Config { config, .. } => config,
        }
    }

    pub fn environment(&self) -> &str {
        match self {
            Self::Path(_) => "*",
            Self::Config {
                environment: Some(environment),
                ..
            } => environment,
            Self::Config {
                environment: None, ..
            } => "*",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct NestedJar {
    pub file: String,
}

#[derive(Debug)]
pub struct DiscoveredMod {
    pub source: PathBuf,
    pub file_name: String,
    pub metadata: FabricMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Ok,
}

#[derive(Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug)]
pub struct ScanReport {
    pub jar_count: usize,
    pub mods: Vec<DiscoveredMod>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn scan_directory(directory: &Path, server_only: bool) -> Result<ScanReport, String> {
    if !directory.is_dir() {
        return Err(format!(
            "mods directory does not exist or is not a directory: {}",
            directory.display()
        ));
    }

    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot list directory {}: {error}", directory.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
        .collect();
    paths.sort();

    let mut mods = Vec::new();
    let mut diagnostics = Vec::new();
    for path in &paths {
        let metadata = match read_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!("{}: {error}", file_name(path)),
                });
                continue;
            }
        };

        let validation_errors = metadata.validate(path);
        if !validation_errors.is_empty() {
            diagnostics.extend(validation_errors.into_iter().map(|message| Diagnostic {
                severity: Severity::Error,
                message,
            }));
            continue;
        }
        if server_only && metadata.environment.as_deref() == Some("client") {
            diagnostics.push(Diagnostic {
                severity: Severity::Info,
                message: format!(
                    "skipped client-only mod '{}' ({})",
                    metadata.id,
                    file_name(path)
                ),
            });
            continue;
        }

        mods.push(DiscoveredMod {
            source: path.clone(),
            file_name: file_name(path),
            metadata,
        });
    }

    check_compatibility(&mods, &mut diagnostics);
    Ok(ScanReport {
        jar_count: paths.len(),
        mods,
        diagnostics,
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn read_metadata(path: &Path) -> Result<FabricMetadata, String> {
    let file = File::open(path).map_err(|error| format!("cannot open: {error}"))?;
    validate_archive_size(
        file.metadata()
            .map_err(|error| format!("cannot inspect archive size: {error}"))?
            .len(),
    )?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("not a readable JAR/ZIP: {error}"))?;
    let mut entry = archive
        .by_name("fabric.mod.json")
        .map_err(|_| "missing fabric.mod.json at JAR root".to_string())?;
    if entry.size() > MAX_METADATA_SIZE {
        return Err(format!(
            "fabric.mod.json exceeds the {MAX_METADATA_SIZE}-byte metadata limit"
        ));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .by_ref()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read metadata: {error}"))?;
    if bytes.len() as u64 > MAX_METADATA_SIZE {
        return Err(format!(
            "fabric.mod.json exceeds the {MAX_METADATA_SIZE}-byte metadata limit"
        ));
    }
    let json = String::from_utf8(bytes)
        .map_err(|error| format!("fabric.mod.json is not UTF-8: {error}"))?;
    serde_json::from_str(&json).map_err(|error| format!("invalid fabric.mod.json: {error}"))
}

fn validate_archive_size(size: u64) -> Result<(), String> {
    if size > MAX_ARCHIVE_SIZE {
        return Err(format!(
            "JAR exceeds the {MAX_ARCHIVE_SIZE}-byte archive limit"
        ));
    }
    Ok(())
}

fn valid_mod_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(byte))
}

fn valid_environment(environment: &str) -> bool {
    matches!(environment, "*" | "client" | "server")
}

fn valid_archive_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return false;
    }
    path.split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[derive(Debug)]
struct PredicateTerm {
    operator: Operator,
    version: String,
    wildcard: Option<Vec<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Equal,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
    Tilde,
    Caret,
}

impl PredicateTerm {
    fn matches(&self, installed_version: &str) -> bool {
        if let Some(prefix) = &self.wildcard {
            let Some(installed) = SemanticVersion::parse(installed_version) else {
                return false;
            };
            if prefix.is_empty() {
                return true;
            }
            let lower = SemanticVersion {
                components: prefix.clone(),
                prerelease: None,
            };
            if installed.compare(&lower) == Ordering::Less {
                return false;
            }
            if prefix.len() == 1 {
                return installed.components[0] == prefix[0];
            }
            if prefix.len() == 2 {
                return installed.components[0] == prefix[0]
                    && installed.components[1] == prefix[1];
            }

            let mut upper_components = prefix.clone();
            let last = upper_components.len() - 1;
            upper_components[last] = upper_components[last].saturating_add(1);
            let upper = SemanticVersion {
                components: upper_components,
                prerelease: None,
            };
            let Some(installed) = SemanticVersion::parse(installed_version) else {
                return false;
            };
            return installed.compare(&lower) != Ordering::Less
                && installed.compare(&upper) == Ordering::Less;
        }

        let installed_semver = SemanticVersion::parse(installed_version);
        let required_semver = SemanticVersion::parse(&self.version);
        match (installed_semver, required_semver) {
            (Some(installed), Some(required)) => {
                let comparison = installed.compare(&required);
                match self.operator {
                    Operator::Equal => comparison == Ordering::Equal,
                    Operator::Greater => comparison == Ordering::Greater,
                    Operator::GreaterEqual => comparison != Ordering::Less,
                    Operator::Less => comparison == Ordering::Less,
                    Operator::LessEqual => comparison != Ordering::Greater,
                    Operator::Tilde => {
                        comparison != Ordering::Less
                            && installed.components[0] == required.components[0]
                            && installed.components[1] == required.components[1]
                    }
                    Operator::Caret => {
                        comparison != Ordering::Less
                            && installed.components[0] == required.components[0]
                    }
                }
            }
            _ => match self.operator {
                Operator::Equal
                | Operator::GreaterEqual
                | Operator::LessEqual
                | Operator::Tilde
                | Operator::Caret => installed_version == self.version,
                Operator::Greater | Operator::Less => false,
            },
        }
    }
}

fn parse_predicate(predicate: &str) -> Result<Vec<PredicateTerm>, String> {
    if predicate.is_empty() || predicate == "*" {
        return Ok(Vec::new());
    }

    let mut terms = Vec::new();
    for token in predicate.split_whitespace() {
        if token == "*" {
            continue;
        }

        let (operator, version) = if let Some(version) = token.strip_prefix(">=") {
            (Operator::GreaterEqual, version)
        } else if let Some(version) = token.strip_prefix("<=") {
            (Operator::LessEqual, version)
        } else if let Some(version) = token.strip_prefix('>') {
            (Operator::Greater, version)
        } else if let Some(version) = token.strip_prefix('<') {
            (Operator::Less, version)
        } else if let Some(version) = token.strip_prefix('=') {
            (Operator::Equal, version)
        } else if let Some(version) = token.strip_prefix('~') {
            (Operator::Tilde, version)
        } else if let Some(version) = token.strip_prefix('^') {
            (Operator::Caret, version)
        } else {
            (Operator::Equal, token)
        };

        if version.is_empty() {
            return Err(format!("missing version after operator in '{token}'"));
        }
        if let Some(wildcard) = parse_wildcard(version)? {
            if operator != Operator::Equal {
                return Err(format!(
                    "wildcard version '{version}' cannot be combined with an operator"
                ));
            }
            terms.push(PredicateTerm {
                operator,
                version: String::new(),
                wildcard: Some(wildcard),
            });
            continue;
        }
        if !valid_version_predicate_value(version) {
            return Err(format!("invalid version value '{version}'"));
        }
        if matches!(operator, Operator::Tilde | Operator::Caret) {
            let Some(parsed_version) = SemanticVersion::parse(version) else {
                return Err(format!("operator in '{token}' requires a semantic version"));
            };
            if operator == Operator::Tilde && parsed_version.components.len() < 2 {
                return Err(format!(
                    "operator in '{token}' requires at least two version components"
                ));
            }
        }
        terms.push(PredicateTerm {
            operator,
            version: version.to_string(),
            wildcard: None,
        });
    }
    Ok(terms)
}

fn valid_version_predicate_value(version: &str) -> bool {
    !version.is_empty()
        && version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+')
        })
}

fn parse_wildcard(version: &str) -> Result<Option<Vec<u64>>, String> {
    let lower = version.to_ascii_lowercase();
    if !lower.contains('x') && !lower.contains('*') {
        return Ok(None);
    }
    let components: Vec<&str> = version.split('.').collect();
    if components.is_empty()
        || !matches!(components.last(), Some(&"x" | &"X" | &"*"))
        || components[..components.len() - 1]
            .iter()
            .any(|component| component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(format!("invalid wildcard version '{version}'"));
    }
    if components.len() == 1 {
        return Err(format!(
            "wildcard version '{version}' must include a major version"
        ));
    }
    let numeric = components[..components.len() - 1]
        .iter()
        .map(|component| {
            component
                .parse::<u64>()
                .map_err(|_| format!("wildcard version component overflows in '{version}'"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(numeric))
}

#[derive(Debug)]
struct SemanticVersion {
    components: Vec<u64>,
    prerelease: Option<String>,
}

impl SemanticVersion {
    fn parse(version: &str) -> Option<Self> {
        let without_build = version.split_once('+').map_or(version, |(value, _)| value);
        let (core, prerelease) = match without_build.split_once('-') {
            Some((core, prerelease)) if !prerelease.is_empty() => {
                (core, Some(prerelease.to_string()))
            }
            Some(_) => return None,
            None => (without_build, None),
        };
        let components: Vec<u64> = core
            .split('.')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        if components.is_empty() {
            return None;
        }
        Some(Self {
            components,
            prerelease,
        })
    }

    fn compare(&self, other: &Self) -> Ordering {
        let component_count = self.components.len().max(other.components.len());
        for index in 0..component_count {
            let left = self.components.get(index).copied().unwrap_or(0);
            let right = other.components.get(index).copied().unwrap_or(0);
            match left.cmp(&right) {
                Ordering::Equal => {}
                difference => return difference,
            }
        }

        match (&self.prerelease, &other.prerelease) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare_prerelease(left, right),
        }
    }
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let left_parts: Vec<&str> = left.split('.').collect();
    let right_parts: Vec<&str> = right.split('.').collect();
    for (left_part, right_part) in left_parts.iter().zip(&right_parts) {
        let comparison = match (left_part.parse::<u64>(), right_part.parse::<u64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => left_part.cmp(right_part),
        };
        if comparison != Ordering::Equal {
            return comparison;
        }
    }
    left_parts.len().cmp(&right_parts.len())
}

fn check_compatibility(mods: &[DiscoveredMod], diagnostics: &mut Vec<Diagnostic>) {
    let mut by_id: BTreeMap<&str, Vec<&DiscoveredMod>> = BTreeMap::new();
    for discovered in mods {
        by_id
            .entry(discovered.metadata.id.as_str())
            .or_default()
            .push(discovered);
    }
    for (id, duplicates) in &by_id {
        if duplicates.len() > 1 {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                message: format!("duplicate mod ID '{id}' in {} JAR files", duplicates.len()),
            });
        }
    }

    for discovered in mods {
        let metadata = &discovered.metadata;
        check_dependencies(
            metadata,
            "depends",
            metadata.depends.as_ref(),
            &by_id,
            Severity::Error,
            true,
            diagnostics,
        );
        check_dependencies(
            metadata,
            "recommends",
            metadata.recommends.as_ref(),
            &by_id,
            Severity::Info,
            false,
            diagnostics,
        );
        check_dependencies(
            metadata,
            "suggests",
            metadata.suggests.as_ref(),
            &by_id,
            Severity::Info,
            false,
            diagnostics,
        );
        check_dependencies(
            metadata,
            "conflicts",
            metadata.conflicts.as_ref(),
            &by_id,
            Severity::Warning,
            false,
            diagnostics,
        );
        check_dependencies(
            metadata,
            "breaks",
            metadata.breaks.as_ref(),
            &by_id,
            Severity::Error,
            false,
            diagnostics,
        );
    }
}

fn check_dependencies(
    metadata: &FabricMetadata,
    field: &str,
    dependencies: Option<&BTreeMap<String, VersionRequirement>>,
    by_id: &BTreeMap<&str, Vec<&DiscoveredMod>>,
    installed_severity: Severity,
    required: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(dependencies) = dependencies else {
        return;
    };
    for (dependency_id, requirement) in dependencies {
        let Some(installed) = by_id.get(dependency_id.as_str()) else {
            if required {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "{} requires missing mod '{}' (version requirement: {})",
                        metadata.id, dependency_id, requirement
                    ),
                });
            } else if matches!(field, "recommends" | "suggests") {
                diagnostics.push(Diagnostic {
                    severity: Severity::Info,
                    message: format!("{} {field} mod '{dependency_id}'", metadata.id),
                });
            }
            continue;
        };

        for candidate in installed {
            let matches = match requirement.matches(&candidate.metadata.version) {
                Ok(matches) => matches,
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Error,
                        message: format!(
                            "{} has invalid {field} requirement for '{}': {error}",
                            metadata.id, dependency_id
                        ),
                    });
                    continue;
                }
            };
            if field == "conflicts" || field == "breaks" {
                if matches {
                    diagnostics.push(Diagnostic {
                        severity: installed_severity,
                        message: format!(
                            "{} declares {field} with installed mod '{}' {}",
                            metadata.id, dependency_id, candidate.metadata.version
                        ),
                    });
                }
            } else if matches {
                diagnostics.push(Diagnostic {
                    severity: Severity::Ok,
                    message: format!(
                        "{} dependency '{}' is present at {} (matches {})",
                        metadata.id, dependency_id, candidate.metadata.version, requirement
                    ),
                });
            } else if required {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    message: format!(
                        "{} requires mod '{}' matching {}, but found {}",
                        metadata.id, dependency_id, requirement, candidate.metadata.version
                    ),
                });
            } else {
                diagnostics.push(Diagnostic {
                    severity: Severity::Info,
                    message: format!(
                        "{} {field} mod '{}' version {}; requirement {} is not met",
                        metadata.id, dependency_id, candidate.metadata.version, requirement
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Cursor, Write},
        time::{SystemTime, UNIX_EPOCH},
    };
    use zip::{write::SimpleFileOptions, ZipWriter};

    fn make_jar(metadata: &str) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut jar = ZipWriter::new(cursor);
        jar.start_file("fabric.mod.json", SimpleFileOptions::default())
            .unwrap();
        jar.write_all(metadata.as_bytes()).unwrap();
        jar.finish().unwrap().into_inner()
    }

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("rubber-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn fabric_version_predicates_support_constraints_and_or_alternatives() {
        let exact = VersionRequirement::One("1.2.3".to_string());
        assert!(exact.matches("1.2.3").unwrap());
        assert!(!exact.matches("1.2.4").unwrap());

        let conjunction = VersionRequirement::One(">=1.2.0 <2.0.0".to_string());
        assert!(conjunction.matches("1.9.0").unwrap());
        assert!(!conjunction.matches("2.0.0").unwrap());

        let alternatives =
            VersionRequirement::Any(vec!["<1.0.0".to_string(), ">=2.0.0".to_string()]);
        assert!(alternatives.matches("2.1.0").unwrap());
        assert!(!alternatives.matches("1.5.0").unwrap());

        assert!(VersionRequirement::One("~1.2.3".to_string())
            .matches("1.2.9")
            .unwrap());
        assert!(!VersionRequirement::One("~1.2.3".to_string())
            .matches("1.3.0")
            .unwrap());
        assert!(VersionRequirement::One("^1.2.3".to_string())
            .matches("1.9.0")
            .unwrap());
        assert!(!VersionRequirement::One("^1.2.3".to_string())
            .matches("2.0.0")
            .unwrap());
        assert!(VersionRequirement::One("1.2.x".to_string())
            .matches("1.2.99")
            .unwrap());
        assert!(!VersionRequirement::One("1.2.x".to_string())
            .matches("1.3.0")
            .unwrap());
        assert!(!VersionRequirement::One("1.2.x".to_string())
            .matches("1.2.0-alpha")
            .unwrap());
    }

    #[test]
    fn malformed_predicates_are_reported_instead_of_silently_accepted() {
        assert!(parse_predicate(">= ").is_err());
        assert!(parse_predicate("1.x.3").is_err());
        assert!(parse_predicate(">not-semver").is_ok());
        assert!(!VersionRequirement::One(">not-semver".to_string())
            .matches("z-version")
            .unwrap());
    }

    #[test]
    fn metadata_validation_rejects_invalid_ids_and_environment() {
        let json = r#"{"schemaVersion":1,"id":"Bad ID","version":"1.0","environment":"both"}"#;
        let metadata: FabricMetadata = serde_json::from_str(json).unwrap();
        let errors = metadata.validate(Path::new("bad.jar"));
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|error| error.contains("field 'id'")));
        assert!(errors
            .iter()
            .any(|error| error.contains("field 'environment'")));
    }

    #[test]
    fn metadata_rejects_null_for_fields_that_are_present() {
        let json = r#"{"schemaVersion":1,"id":"null-field","version":"1.0","environment":null}"#;
        assert!(serde_json::from_str::<FabricMetadata>(json).is_err());
    }

    #[test]
    fn accepts_fabric_entrypoint_objects_and_rejects_invalid_declarations() {
        let valid = r#"{
            "schemaVersion":1,
            "id":"entrypoints",
            "version":"1.0",
            "entrypoints":{"main":[{"value":"example.Main","adapter":"default"}]}
        }"#;
        let metadata: FabricMetadata = serde_json::from_str(valid).unwrap();
        assert!(metadata.validate(Path::new("entrypoints.jar")).is_empty());
        assert_eq!(metadata.entrypoint_count(), 1);

        let invalid = r#"{
            "schemaVersion":1,
            "id":"bad-entrypoint",
            "version":"1.0",
            "entrypoints":{"main":[{"adapter":"default"}]}
        }"#;
        let metadata: FabricMetadata = serde_json::from_str(invalid).unwrap();
        assert!(metadata
            .validate(Path::new("bad-entrypoint.jar"))
            .iter()
            .any(|error| error.contains("entrypoints.main")));
    }

    #[test]
    fn scans_fixtures_and_checks_required_dependency_version() {
        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples")
            .join("mods");
        let report = scan_directory(&fixture_dir, true).unwrap();
        assert_eq!(report.jar_count, 3);
        assert_eq!(report.mods.len(), 2);
        assert!(report
            .mods
            .iter()
            .any(|item| item.metadata.id == "demo-addon"));
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Ok
                && diagnostic.message.contains("demo-core")
                && diagnostic.message.contains("matches >=1.0.0")
        }));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("skipped client-only") }));
    }

    #[test]
    fn invalid_and_oversized_archives_are_isolated_and_diagnosed() {
        let directory = temp_dir();
        fs::write(directory.join("invalid.jar"), b"not a JAR").unwrap();
        fs::write(directory.join("malformed.jar"), make_jar("{broken")).unwrap();
        let oversized = format!(
            r#"{{"schemaVersion":1,"id":"oversized","version":"1.0","name":"{}"}}"#,
            "x".repeat(MAX_METADATA_SIZE as usize)
        );
        fs::write(directory.join("oversized.jar"), make_jar(&oversized)).unwrap();

        let report = scan_directory(&directory, false).unwrap();
        assert_eq!(report.jar_count, 3);
        assert!(report.mods.is_empty());
        assert_eq!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .count(),
            3
        );
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("metadata limit")));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("invalid fabric.mod.json") }));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn archive_size_limit_is_enforced_before_zip_parsing() {
        assert!(validate_archive_size(MAX_ARCHIVE_SIZE).is_ok());
        assert!(validate_archive_size(MAX_ARCHIVE_SIZE + 1)
            .unwrap_err()
            .contains("archive limit"));
    }

    #[test]
    fn reports_required_dependency_version_mismatch() {
        let directory = temp_dir();
        fs::write(
            directory.join("needs.jar"),
            make_jar(
                r#"{"schemaVersion":1,"id":"needs","version":"1","depends":{"library":">=2.0.0"}}"#,
            ),
        )
        .unwrap();
        fs::write(
            directory.join("library.jar"),
            make_jar(r#"{"schemaVersion":1,"id":"library","version":"1.4.0"}"#),
        )
        .unwrap();

        let report = scan_directory(&directory, false).unwrap();
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.message.contains("requires mod 'library'")
                && diagnostic.message.contains("but found 1.4.0")
        }));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reports_missing_dependencies_duplicates_and_versioned_conflicts() {
        let directory = temp_dir();
        fs::write(
            directory.join("dependent.jar"),
            make_jar(
                r#"{"schemaVersion":1,"id":"dependent","version":"1","depends":{"absent":"*"},"conflicts":{"library":">=2.0.0"}}"#,
            ),
        )
        .unwrap();
        fs::write(
            directory.join("library-one.jar"),
            make_jar(r#"{"schemaVersion":1,"id":"library","version":"2.1.0"}"#),
        )
        .unwrap();
        fs::write(
            directory.join("library-two.jar"),
            make_jar(r#"{"schemaVersion":1,"id":"library","version":"1.0.0"}"#),
        )
        .unwrap();

        let report = scan_directory(&directory, false).unwrap();
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("requires missing mod 'absent'")));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("duplicate mod ID 'library'")));
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic
                    .message
                    .contains("conflicts with installed mod 'library' 2.1.0")
        }));
        fs::remove_dir_all(directory).unwrap();
    }
}
