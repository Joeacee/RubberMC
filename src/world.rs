use fastanvil::Region;
use fastnbt::Value;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LEVEL_DAT_SIZE: u64 = 64 * 1024 * 1024;
const MINECRAFT_26_2_DATA_VERSION: i32 = 4903;
const MINECRAFT_LEVEL_VERSION: i32 = 19133;

pub struct World {
    path: PathBuf,
    level_data: HashMap<String, Value>,
}

impl World {
    pub fn create(path: impl AsRef<Path>, name: &str) -> io::Result<Self> {
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            return Err(invalid_input(
                "world name must be non-empty and contain no control characters",
            ));
        }

        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(path)?;
        for dimension in ["", "DIM-1", "DIM1"] {
            let dimension_path = if dimension.is_empty() {
                path.to_path_buf()
            } else {
                path.join(dimension)
            };
            for directory in ["region", "entities", "poi"] {
                fs::create_dir_all(dimension_path.join(directory))?;
            }
        }
        fs::create_dir_all(path.join("data"))?;
        fs::create_dir_all(path.join("playerdata"))?;

        let now = current_time_millis()?;
        let seed = uuid::Uuid::new_v4().as_u128() as i64;
        let level_data = initial_level_data(name, now);

        let world = Self {
            path: path.to_path_buf(),
            level_data,
        };
        world.save_level_data()?;
        save_world_gen_settings(path, seed)?;
        let mut lock = File::create(path.join("session.lock"))?;
        lock.write_all(&now.to_be_bytes())?;
        lock.sync_all()?;
        Ok(world)
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let level_data = read_level_data(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            level_data,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn level_data(&self) -> &HashMap<String, Value> {
        &self.level_data
    }

    pub fn level_data_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.level_data
    }

    pub fn save_level_data(&self) -> io::Result<()> {
        let mut root = HashMap::new();
        root.insert("Data".to_owned(), Value::Compound(self.level_data.clone()));
        let nbt = fastnbt::to_bytes(&root).map_err(invalid_data)?;
        let temp_path = self.path.join("level.dat_new");
        let level_path = self.path.join("level.dat");
        let backup_path = self.path.join("level.dat_old");

        let file = File::create(&temp_path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&nbt)?;
        let file = encoder.finish()?;
        file.sync_all()?;

        if level_path.exists() {
            match fs::remove_file(&backup_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            fs::rename(&level_path, &backup_path)?;
        }
        if let Err(error) = fs::rename(&temp_path, &level_path) {
            if backup_path.exists() {
                if let Err(restore_error) = fs::rename(&backup_path, &level_path) {
                    return Err(io::Error::new(
                        restore_error.kind(),
                        format!("{error}; restoring level.dat_old also failed: {restore_error}"),
                    ));
                }
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn read_chunk(&self, chunk_x: i32, chunk_z: i32) -> io::Result<Option<Vec<u8>>> {
        let (region_path, local_x, local_z) = region_location(&self.path, chunk_x, chunk_z);
        if !region_path.exists() {
            return Ok(None);
        }
        let file = File::open(region_path)?;
        let mut region = Region::from_stream(file).map_err(invalid_data)?;
        let chunk = region.read_chunk(local_x, local_z).map_err(invalid_data)?;
        if let Some(nbt) = &chunk {
            validate_chunk_nbt(nbt)?;
        }
        Ok(chunk)
    }

    pub fn write_chunk(&self, chunk_x: i32, chunk_z: i32, nbt: &[u8]) -> io::Result<()> {
        validate_chunk_nbt(nbt)?;
        let (region_path, local_x, local_z) = region_location(&self.path, chunk_x, chunk_z);
        let file = if region_path.exists() {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(region_path)?
        } else {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(region_path)?
        };
        let mut region = if file.metadata()?.len() == 0 {
            Region::create(file).map_err(invalid_data)?
        } else {
            Region::from_stream(file).map_err(invalid_data)?
        };
        region
            .write_chunk(local_x, local_z, nbt)
            .map_err(invalid_data)?;
        let mut file = region.into_inner()?;
        let end = file.stream_position()?;
        file.set_len(end)?;
        file.sync_all()
    }

    pub fn remove_chunk(&self, chunk_x: i32, chunk_z: i32) -> io::Result<()> {
        let (region_path, local_x, local_z) = region_location(&self.path, chunk_x, chunk_z);
        if !region_path.exists() {
            return Ok(());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(region_path)?;
        let mut region = Region::from_stream(file).map_err(invalid_data)?;
        region
            .remove_chunk(local_x, local_z)
            .map_err(invalid_data)?;
        let mut file = region.into_inner()?;
        let end = file.stream_position()?;
        file.set_len(end)?;
        file.sync_all()
    }

    pub fn region_file_count(&self) -> io::Result<usize> {
        let region_dir = self.path.join("region");
        let mut count = 0;
        for entry in fs::read_dir(region_dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "mca")
            {
                count += 1;
            }
        }
        Ok(count)
    }
}

fn read_level_data(path: &Path) -> io::Result<HashMap<String, Value>> {
    let level_path = path.join("level.dat");
    let backup_path = path.join("level.dat_old");
    match read_level_file(&level_path) {
        Ok(data) => Ok(data),
        Err(primary_error) if backup_path.exists() => read_level_file(&backup_path).map_err(
            |backup_error| {
                io::Error::new(
                    backup_error.kind(),
                    format!(
                        "could not read level.dat ({primary_error}); level.dat_old also failed: {backup_error}"
                    ),
                )
            },
        ),
        Err(error) => Err(error),
    }
}

fn initial_level_data(name: &str, last_played: i64) -> HashMap<String, Value> {
    let mut version = HashMap::new();
    version.insert("Name".to_owned(), Value::String("26.2".to_owned()));
    version.insert("Id".to_owned(), Value::Int(MINECRAFT_26_2_DATA_VERSION));
    version.insert("Snapshot".to_owned(), Value::Byte(0));
    version.insert("Series".to_owned(), Value::String("main".to_owned()));

    let mut data_packs = HashMap::new();
    data_packs.insert(
        "Enabled".to_owned(),
        Value::List(vec![Value::String("vanilla".to_owned())]),
    );
    data_packs.insert("Disabled".to_owned(), Value::List(Vec::new()));

    HashMap::from([
        ("allowCommands".to_owned(), Value::Byte(0)),
        ("BorderCenterX".to_owned(), Value::Double(0.0)),
        ("BorderCenterZ".to_owned(), Value::Double(0.0)),
        ("BorderDamagePerBlock".to_owned(), Value::Double(0.2)),
        ("BorderSize".to_owned(), Value::Double(60_000_000.0)),
        ("BorderSafeZone".to_owned(), Value::Double(5.0)),
        (
            "BorderSizeLerpTarget".to_owned(),
            Value::Double(60_000_000.0),
        ),
        ("BorderSizeLerpTime".to_owned(), Value::Long(0)),
        ("BorderWarningBlocks".to_owned(), Value::Double(5.0)),
        ("BorderWarningTime".to_owned(), Value::Double(15.0)),
        ("DataPacks".to_owned(), Value::Compound(data_packs)),
        (
            "DataVersion".to_owned(),
            Value::Int(MINECRAFT_26_2_DATA_VERSION),
        ),
        ("Difficulty".to_owned(), Value::Byte(2)),
        ("DifficultyLocked".to_owned(), Value::Byte(0)),
        ("GameType".to_owned(), Value::Int(0)),
        ("initialized".to_owned(), Value::Byte(1)),
        ("LastPlayed".to_owned(), Value::Long(last_played)),
        ("LevelName".to_owned(), Value::String(name.to_owned())),
        ("SpawnX".to_owned(), Value::Int(0)),
        ("SpawnY".to_owned(), Value::Int(64)),
        ("SpawnZ".to_owned(), Value::Int(0)),
        ("Time".to_owned(), Value::Long(0)),
        ("DayTime".to_owned(), Value::Long(0)),
        ("Version".to_owned(), Value::Compound(version)),
        ("version".to_owned(), Value::Int(MINECRAFT_LEVEL_VERSION)),
    ])
}

fn save_world_gen_settings(path: &Path, seed: i64) -> io::Result<()> {
    let mut dimensions = HashMap::new();
    for (name, dimension_type, settings, biome_preset, biome_type) in [
        (
            "minecraft:overworld",
            "minecraft:overworld",
            "minecraft:overworld",
            Some("minecraft:overworld"),
            "minecraft:multi_noise",
        ),
        (
            "minecraft:the_nether",
            "minecraft:the_nether",
            "minecraft:nether",
            Some("minecraft:nether"),
            "minecraft:multi_noise",
        ),
        (
            "minecraft:the_end",
            "minecraft:the_end",
            "minecraft:end",
            None,
            "minecraft:the_end",
        ),
    ] {
        let mut generator = HashMap::new();
        generator.insert(
            "type".to_owned(),
            Value::String("minecraft:noise".to_owned()),
        );
        generator.insert("settings".to_owned(), Value::String(settings.to_owned()));
        let mut biome_source = HashMap::new();
        biome_source.insert("type".to_owned(), Value::String(biome_type.to_owned()));
        if let Some(preset) = biome_preset {
            biome_source.insert("preset".to_owned(), Value::String(preset.to_owned()));
        }
        generator.insert("biome_source".to_owned(), Value::Compound(biome_source));

        let mut dimension = HashMap::new();
        dimension.insert("type".to_owned(), Value::String(dimension_type.to_owned()));
        dimension.insert("generator".to_owned(), Value::Compound(generator));
        dimensions.insert(name.to_owned(), Value::Compound(dimension));
    }
    let mut settings = HashMap::new();
    settings.insert(
        "DataVersion".to_owned(),
        Value::Int(MINECRAFT_26_2_DATA_VERSION),
    );
    settings.insert("seed".to_owned(), Value::Long(seed));
    settings.insert("generate_structures".to_owned(), Value::Byte(1));
    settings.insert("bonus_chest".to_owned(), Value::Byte(0));
    settings.insert("dimensions".to_owned(), Value::Compound(dimensions));
    let root = HashMap::from([("data".to_owned(), Value::Compound(settings))]);
    let bytes = fastnbt::to_bytes(&root).map_err(invalid_data)?;

    let dir = path.join("data").join("minecraft");
    fs::create_dir_all(&dir)?;
    let temp_path = dir.join("world_gen_settings.dat_new");
    let output_path = dir.join("world_gen_settings.dat");
    let file = File::create(&temp_path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&bytes)?;
    let file = encoder.finish()?;
    file.sync_all()?;
    fs::rename(temp_path, output_path)
}

fn read_level_file(path: &Path) -> io::Result<HashMap<String, Value>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_LEVEL_DAT_SIZE {
        return Err(invalid_data("level.dat exceeds the 64 MiB size limit"));
    }
    let mut decoder = GzDecoder::new(file).take(MAX_LEVEL_DAT_SIZE + 1);
    let mut nbt = Vec::new();
    decoder.read_to_end(&mut nbt)?;
    if nbt.len() as u64 > MAX_LEVEL_DAT_SIZE {
        return Err(invalid_data(
            "decompressed level.dat exceeds the 64 MiB size limit",
        ));
    }
    let root: HashMap<String, Value> = fastnbt::from_bytes(&nbt).map_err(invalid_data)?;
    match root.get("Data") {
        Some(Value::Compound(data)) => Ok(data.clone()),
        _ => Err(invalid_data("level.dat root must contain a Data compound")),
    }
}

fn validate_chunk_nbt(nbt: &[u8]) -> io::Result<()> {
    let _: HashMap<String, Value> = fastnbt::from_bytes(nbt).map_err(invalid_data)?;
    Ok(())
}

fn region_location(world_path: &Path, chunk_x: i32, chunk_z: i32) -> (PathBuf, usize, usize) {
    let region_x = chunk_x.div_euclid(32);
    let region_z = chunk_z.div_euclid(32);
    let local_x = chunk_x.rem_euclid(32) as usize;
    let local_z = chunk_z.rem_euclid(32) as usize;
    (
        world_path
            .join("region")
            .join(format!("r.{region_x}.{region_z}.mca")),
        local_x,
        local_z,
    )
}

fn current_time_millis() -> io::Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    i64::try_from(millis).map_err(io::Error::other)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_world_path() -> PathBuf {
        std::env::temp_dir().join(format!("rubber-world-{}", uuid::Uuid::new_v4()))
    }

    fn test_chunk_nbt(value: i32) -> Vec<u8> {
        let mut chunk = HashMap::new();
        chunk.insert("xPos".to_owned(), Value::Int(value));
        fastnbt::to_bytes(&chunk).unwrap()
    }

    #[test]
    fn create_and_reopen_world_preserves_level_data() {
        let path = temporary_world_path();
        let result = (|| {
            let mut world = World::create(&path, "Test World")?;
            assert_eq!(
                world.level_data().get("LevelName"),
                Some(&Value::String("Test World".to_owned()))
            );
            assert!(path.join("region").is_dir());
            assert!(path.join("DIM-1").join("region").is_dir());
            assert!(path.join("DIM1").join("region").is_dir());
            assert!(path
                .join("data")
                .join("minecraft")
                .join("world_gen_settings.dat")
                .is_file());
            assert_eq!(
                world.level_data().get("DataVersion"),
                Some(&Value::Int(MINECRAFT_26_2_DATA_VERSION))
            );
            assert_eq!(
                world.level_data().get("version"),
                Some(&Value::Int(MINECRAFT_LEVEL_VERSION))
            );
            assert_eq!(
                world.level_data().get("Version"),
                Some(&Value::Compound(HashMap::from([
                    ("Name".to_owned(), Value::String("26.2".to_owned())),
                    ("Id".to_owned(), Value::Int(MINECRAFT_26_2_DATA_VERSION)),
                    ("Snapshot".to_owned(), Value::Byte(0)),
                    ("Series".to_owned(), Value::String("main".to_owned())),
                ])))
            );
            let mut decoder = GzDecoder::new(File::open(
                path.join("data/minecraft/world_gen_settings.dat"),
            )?);
            let mut settings_bytes = Vec::new();
            decoder.read_to_end(&mut settings_bytes)?;
            let settings_root: HashMap<String, Value> =
                fastnbt::from_bytes(&settings_bytes).map_err(invalid_data)?;
            let Value::Compound(world_gen_settings) = &settings_root["data"] else {
                panic!("world generation data must be stored in the data compound");
            };
            assert_eq!(
                world_gen_settings.get("DataVersion"),
                Some(&Value::Int(MINECRAFT_26_2_DATA_VERSION))
            );
            let Value::Compound(dimensions) = &world_gen_settings["dimensions"] else {
                panic!("world generation settings must contain dimensions");
            };
            assert_eq!(dimensions.len(), 3);
            assert!(dimensions.contains_key("minecraft:overworld"));
            assert!(dimensions.contains_key("minecraft:the_nether"));
            assert!(dimensions.contains_key("minecraft:the_end"));

            world
                .level_data_mut()
                .insert("Time".to_owned(), Value::Long(1200));
            world.save_level_data()?;
            let reopened = World::open(&path)?;
            assert_eq!(reopened.level_data().get("Time"), Some(&Value::Long(1200)));
            Ok::<_, io::Error>(())
        })();
        let _ = fs::remove_dir_all(path);
        result.unwrap();
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_directory() {
        let path = temporary_world_path();
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("keep.txt"), b"keep").unwrap();
        let result = World::create(&path, "Must not replace");
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(path.join("keep.txt")).unwrap(), b"keep");
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn chunk_round_trip_uses_anvil_region_coordinates() {
        let path = temporary_world_path();
        let result = (|| {
            let world = World::create(&path, "Chunks")?;
            let nbt = test_chunk_nbt(-17);
            world.write_chunk(-1, 33, &nbt)?;
            assert_eq!(world.read_chunk(-1, 33)?, Some(nbt));
            assert!(path.join("region").join("r.-1.1.mca").is_file());
            assert_eq!(world.region_file_count()?, 1);

            world.write_chunk(-1, 33, &test_chunk_nbt(42))?;
            let replacement = world.read_chunk(-1, 33)?.unwrap();
            let values: HashMap<String, Value> = fastnbt::from_bytes(&replacement).unwrap();
            assert_eq!(values.get("xPos"), Some(&Value::Int(42)));
            Ok::<_, io::Error>(())
        })();
        let _ = fs::remove_dir_all(path);
        result.unwrap();
    }

    #[test]
    fn remove_chunk_and_reject_invalid_nbt() {
        let path = temporary_world_path();
        let result = (|| {
            let world = World::create(&path, "Remove")?;
            assert!(world.write_chunk(0, 0, b"not NBT").is_err());
            world.write_chunk(0, 0, &test_chunk_nbt(1))?;
            world.remove_chunk(0, 0)?;
            assert_eq!(world.read_chunk(0, 0)?, None);
            Ok::<_, io::Error>(())
        })();
        let _ = fs::remove_dir_all(path);
        result.unwrap();
    }

    #[test]
    fn open_recovers_level_data_from_backup() {
        let path = temporary_world_path();
        let result = (|| {
            let world = World::create(&path, "Backup")?;
            world.save_level_data()?;
            fs::write(path.join("level.dat"), b"corrupt")?;
            assert_eq!(
                World::open(&path)?.level_data().get("LevelName"),
                Some(&Value::String("Backup".to_owned()))
            );
            drop(world);
            Ok::<_, io::Error>(())
        })();
        let _ = fs::remove_dir_all(path);
        result.unwrap();
    }
}
