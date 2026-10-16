use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process,
};
use zip::ZipArchive;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FabricMetadata {
    schema_version: Option<u32>,
    id: String,
    version: String,
    name: Option<String>,
    environment: Option<String>,
    depends: Option<BTreeMap<String, VersionRequirement>>,
    recommends: Option<BTreeMap<String, VersionRequirement>>,
    breaks: Option<BTreeMap<String, VersionRequirement>>,
    conflicts: Option<BTreeMap<String, VersionRequirement>>,
    mixins: Option<Vec<MixinEntry>>,
    entrypoints: Option<BTreeMap<String, serde_json::Value>>,
    access_widener: Option<String>,
    jars: Option<Vec<NestedJar>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum VersionRequirement {
    One(String),
    Many(Vec<String>),
    Other(serde_json::Value),
}

impl std::fmt::Display for VersionRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::One(s) => write!(f, "{s}"),
            Self::Many(v) => write!(f, "{}", v.join(" OR ")),
            Self::Other(v) => write!(f, "{v}"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MixinEntry {
    Path(String),
    Config { config: String, environment: Option<String> },
}

impl MixinEntry {
    fn config(&self) -> &str {
        match self { Self::Path(s) => s, Self::Config { config, .. } => config }
    }
    fn environment(&self) -> &str {
        match self { Self::Path(_) => "*", Self::Config { environment: Some(e), .. } => e, Self::Config { environment: None, .. } => "*" }
    }
}

#[derive(Debug, Deserialize)]
struct NestedJar { file: String }

#[derive(Debug)]
struct DiscoveredMod {
    source: PathBuf,
    metadata: FabricMetadata,
}

fn read_metadata(path: &Path) -> Result<FabricMetadata, String> {
    let file = File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("not a readable JAR/ZIP: {e}"))?;
    let mut entry = archive.by_name("fabric.mod.json")
        .map_err(|_| "missing fabric.mod.json at JAR root".to_string())?;
    let mut json = String::new();
    entry.read_to_string(&mut json).map_err(|e| format!("cannot read metadata: {e}"))?;
    serde_json::from_str(&json).map_err(|e| format!("invalid fabric.mod.json: {e}"))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("RustFabric prototype v{}", env!("CARGO_PKG_VERSION"));
        println!("Usage: rustabric-prototype <mods-directory> [--server]");
        println!("Scans .jar metadata; does not execute Java mod code yet.");
        return;
    }

    let mods_dir = PathBuf::from(&args[1]);
    let server_only = args.iter().any(|a| a == "--server");
    if !mods_dir.is_dir() {
        eprintln!("error: mods directory does not exist: {}", mods_dir.display());
        process::exit(2);
    }

    let entries = match fs::read_dir(&mods_dir) {
        Ok(entries) => entries,
        Err(e) => { eprintln!("error: cannot read directory: {e}"); process::exit(2); }
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("jar")))
        .collect();
    paths.sort();

    println!("RustFabric prototype — metadata discovery");
    println!("Directory: {}", mods_dir.display());
    println!("Mode: {}", if server_only { "server" } else { "all environments" });
    println!("JAR files found: {}\n", paths.len());

    let mut mods = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        match read_metadata(&path) {
            Ok(metadata) => {
                if metadata.schema_version != Some(1) {
                    errors.push(format!("{}: unsupported or missing schemaVersion {:?}", path.display(), metadata.schema_version));
                    continue;
                }
                if server_only && metadata.environment.as_deref() == Some("client") {
                    println!("[SKIP client-only] {} ({})", metadata.id, path.file_name().unwrap_or_default().to_string_lossy());
                    continue;
                }
                mods.push(DiscoveredMod { source: path, metadata });
            }
            Err(e) => errors.push(format!("{}: {e}", path.file_name().unwrap_or_default().to_string_lossy())),
        }
    }

    let mut by_id: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, m) in mods.iter().enumerate() { by_id.entry(m.metadata.id.clone()).or_default().push(i); }

    println!("DISCOVERED MODS ({})", mods.len());
    for m in &mods {
        let md = &m.metadata;
        println!("\n• {} {} — {}", md.id, md.version, md.name.as_deref().unwrap_or(&md.id));
        println!("  File: {}", m.source.file_name().unwrap_or_default().to_string_lossy());
        println!("  Environment: {}", md.environment.as_deref().unwrap_or("* (default)"));
        let entrypoint_count: usize = md.entrypoints.as_ref().map(|map| map.values().map(|v| match v { serde_json::Value::Array(a) => a.len(), _ => 1 }).sum()).unwrap_or(0);
        println!("  Entrypoints declared: {entrypoint_count} (not executed)");
        if let Some(mixins) = &md.mixins {
            for mixin in mixins { println!("  Mixin config: {} [environment: {}]", mixin.config(), mixin.environment()); }
        }
        if let Some(aw) = &md.access_widener { println!("  Access widener: {aw} (not applied)"); }
        if let Some(jars) = &md.jars {
            for jar in jars { println!("  Nested JAR: {} (discovery not implemented yet)", jar.file); }
        }
    }

    let mut problems = 0usize;
    println!("\nDEPENDENCY / COMPATIBILITY CHECKS");
    for (id, indexes) in &by_id {
        if indexes.len() > 1 {
            problems += 1;
            println!("[ERROR] duplicate mod ID '{id}' in {} JAR files", indexes.len());
        }
    }
    let installed: BTreeSet<&str> = by_id.keys().map(String::as_str).collect();
    for m in &mods {
        let md = &m.metadata;
        if let Some(deps) = &md.depends {
            for (dep, requirement) in deps {
                if !installed.contains(dep.as_str()) {
                    problems += 1;
                    println!("[ERROR] {} requires missing mod '{}' (declared version: {})", md.id, dep, requirement);
                } else {
                    println!("[OK] {} dependency '{}' is present (version constraint not evaluated yet: {})", md.id, dep, requirement);
                }
            }
        }
        if let Some(conflicts) = &md.conflicts {
            for (other, _) in conflicts {
                if installed.contains(other.as_str()) {
                    problems += 1;
                    println!("[WARN] {} declares a conflict with installed mod '{}'; exact version matching not implemented", md.id, other);
                }
            }
        }
        if let Some(breaks) = &md.breaks {
            for (other, _) in breaks {
                if installed.contains(other.as_str()) {
                    problems += 1;
                    println!("[ERROR] {} declares that it breaks with installed mod '{}'; exact version matching not implemented", md.id, other);
                }
            }
        }
        if let Some(recommends) = &md.recommends {
            for (other, _) in recommends {
                if !installed.contains(other.as_str()) {
                    println!("[INFO] {} recommends mod '{}'", md.id, other);
                }
            }
        }
    }
    for error in errors {
        problems += 1;
        println!("[WARN] {error}");
    }

    println!("\nSUMMARY: {} mod(s), {} reported issue(s).", mods.len(), problems);
    println!("STATUS: metadata prototype only; no Java execution, Fabric API bridge, Mixin transformation, or Minecraft server yet.");
}
