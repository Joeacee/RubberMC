use std::{
    collections::HashSet,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use pumpkin_data::{
    entity_id_remap::remap_entity_id_for_version,
    item_id_remap::remap_item_id_for_version,
    packet::{self, PacketId, CURRENT_MC_VERSION},
    registry::Registry,
    tag::{get_registry_key_tags, RegistryKey},
};

const MINECRAFT_VERSION: &str = "26.2";
const PROTOCOL_VERSION: i32 = 776;
const MAX_PACKET_SIZE: usize = 1024 * 1024;
const HANDSHAKE_STATUS: i32 = 1;
const HANDSHAKE_LOGIN: i32 = 2;
const LOGIN_DISCONNECT_PACKET: i32 = 0;
const TELEPORT_ID: i32 = 1;
const CHUNK_SECTION_COUNT: usize = 24;
const OVERWORLD_MIN_Y: i32 = -64;
const PLATFORM_BLOCK_Y: i32 = 15;
const PLATFORM_SECTION_INDEX: usize = ((PLATFORM_BLOCK_Y - OVERWORLD_MIN_Y) / 16) as usize;
const SPAWN_X: i32 = 8;
const SPAWN_Y: i32 = PLATFORM_BLOCK_Y + 1;
const SPAWN_Z: i32 = 8;
const STONE_BLOCK_STATE_ID: i32 = 1;

pub fn serve(bind_address: &str) -> io::Result<()> {
    let listener = TcpListener::bind(bind_address)?;
    println!("Rubber 26.2 server listening on {}", listener.local_addr()?);
    println!("Offline login and a minimal single-chunk world are enabled.");
    println!("Offline login does not authenticate account ownership.");

    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream) {
                        eprintln!("client connection ended: {error}");
                    }
                });
            }
            Err(error) => eprintln!("failed to accept client connection: {error}"),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let handshake = read_frame(&mut stream)?;
    let mut cursor = 0;
    let packet_id = read_varint(&handshake, &mut cursor)?;
    if packet_id != 0 {
        return Err(invalid_data("expected handshake packet"));
    }
    let client_protocol = read_varint(&handshake, &mut cursor)?;
    let _server_address = read_string(&handshake, &mut cursor)?;
    read_exact(&handshake, &mut cursor, 2)?;
    let next_state = read_varint(&handshake, &mut cursor)?;
    if cursor != handshake.len() {
        return Err(invalid_data("unexpected trailing bytes in handshake"));
    }
    match next_state {
        HANDSHAKE_STATUS => handle_status(&mut stream),
        HANDSHAKE_LOGIN => handle_login(&mut stream, client_protocol),
        _ => Err(invalid_data("unsupported handshake next state")),
    }
}

fn handle_status(stream: &mut TcpStream) -> io::Result<()> {
    loop {
        let frame = match read_frame(stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut cursor = 0;
        let packet_id = read_varint(&frame, &mut cursor)?;
        match packet_id {
            0 if cursor == frame.len() => {
                let response = status_response();
                write_packet(stream, 0, response.as_bytes())?;
            }
            1 if frame.len() - cursor == 8 => {
                let mut pong_payload = Vec::with_capacity(9);
                write_varint(1, &mut pong_payload);
                pong_payload.extend_from_slice(&frame[cursor..]);
                write_frame(stream, &pong_payload)?;
                return Ok(());
            }
            0 => return Err(invalid_data("status request packet must be empty")),
            1 => return Err(invalid_data("ping packet must contain exactly 8 bytes")),
            _ => return Err(invalid_data("unknown status packet")),
        }
    }
}

fn handle_login(stream: &mut TcpStream, client_protocol: i32) -> io::Result<()> {
    if client_protocol != PROTOCOL_VERSION {
        return write_login_disconnect(
            stream,
            &format!(
                "This Rubber development build requires Minecraft {MINECRAFT_VERSION} (protocol {PROTOCOL_VERSION}); received protocol {client_protocol}."
            ),
        );
    }

    let login_start = read_frame(stream)?;
    let mut cursor = 0;
    if read_varint(&login_start, &mut cursor)?
        != packet_id(&packet::serverbound::login::LOGIN_START)?
    {
        return write_login_disconnect(stream, "Expected a login-start packet.");
    }
    let username = match read_bounded_string(&login_start, &mut cursor, 16) {
        Ok(username) => username,
        Err(_) => return write_login_disconnect(stream, "Invalid player name."),
    };
    if !valid_username(&username) {
        return write_login_disconnect(
            stream,
            "Player names must be 3-16 ASCII letters, digits, or underscores.",
        );
    }
    let uuid_bytes = match read_exact(&login_start, &mut cursor, 16) {
        Ok(bytes) => bytes,
        Err(_) => return write_login_disconnect(stream, "Invalid login-start UUID."),
    };
    if cursor != login_start.len() {
        return write_login_disconnect(stream, "Unexpected data in login-start packet.");
    }

    let mut login_success = Vec::with_capacity(username.len() + 40);
    write_varint(
        packet_id(&packet::clientbound::login::LOGIN_SUCCESS)?,
        &mut login_success,
    );
    login_success.extend_from_slice(uuid_bytes);
    write_string(username.as_bytes(), &mut login_success)?;
    write_varint(0, &mut login_success);
    login_success.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    write_frame(stream, &login_success)?;

    let acknowledgement = read_frame(stream)?;
    let mut cursor = 0;
    if read_varint(&acknowledgement, &mut cursor)?
        != packet_id(&packet::serverbound::login::LOGIN_ACKNOWLEDGED)?
        || cursor != acknowledgement.len()
    {
        return write_login_disconnect(stream, "Expected login acknowledgement.");
    }

    eprintln!("Offline login completed for '{username}'; beginning 26.2 configuration.");
    handle_configuration(stream)
}

fn handle_configuration(stream: &mut TcpStream) -> io::Result<()> {
    let mut known_packs = Vec::new();
    write_varint(1, &mut known_packs);
    write_string(b"minecraft", &mut known_packs)?;
    write_string(b"core", &mut known_packs)?;
    write_string(MINECRAFT_VERSION.as_bytes(), &mut known_packs)?;
    write_clientbound_packet(
        stream,
        &packet::clientbound::config::SELECT_KNOWN_PACKS,
        &known_packs,
    )?;

    let mut registries_sent = false;
    loop {
        let frame = read_frame(stream)?;
        let mut cursor = 0;
        let received_id = read_varint(&frame, &mut cursor)?;

        if received_id == packet_id(&packet::serverbound::config::CLIENT_INFORMATION)? {
            read_client_information(&frame, &mut cursor)?;
        } else if received_id == packet_id(&packet::serverbound::config::CUSTOM_PAYLOAD)? {
            read_bounded_string(&frame, &mut cursor, 32_767)?;
        } else if received_id == packet_id(&packet::serverbound::config::SELECT_KNOWN_PACKS)? {
            if registries_sent {
                return Err(invalid_data("received duplicate known-packs selection"));
            }
            read_selected_known_packs(&frame, &mut cursor)?;
            send_configuration_data(stream)?;
            registries_sent = true;
        } else if received_id == packet_id(&packet::serverbound::config::FINISH_CONFIGURATION)? {
            if !registries_sent || cursor != frame.len() {
                return Err(invalid_data(
                    "received finish-configuration before registry synchronization completed",
                ));
            }
            return enter_play(stream);
        } else {
            return Err(invalid_data("unexpected packet during configuration"));
        }
    }
}

fn send_configuration_data(stream: &mut TcpStream) -> io::Result<()> {
    let enabled_features_id = packet::clientbound::config::UPDATE_ENABLED_FEATURES.v26_2;
    if enabled_features_id >= 0 {
        let mut payload = Vec::new();
        write_varint(1, &mut payload);
        write_string(b"minecraft:vanilla", &mut payload)?;
        write_raw_packet(stream, enabled_features_id, &payload)?;
    }

    for registry in Registry::get_synced(CURRENT_MC_VERSION) {
        let mut payload = Vec::new();
        write_string(registry.registry_id.as_bytes(), &mut payload)?;
        write_varint(
            i32::try_from(registry.registry_entries.len())
                .map_err(|_| invalid_data("registry entry count exceeds VarInt range"))?,
            &mut payload,
        );
        for entry in registry.registry_entries {
            write_string(entry.entry_id.as_bytes(), &mut payload)?;
            match entry.data {
                Some(data) => {
                    payload.push(1);
                    payload.extend_from_slice(&data);
                }
                None => payload.push(0),
            }
        }
        write_clientbound_packet(
            stream,
            &packet::clientbound::config::REGISTRY_DATA,
            &payload,
        )?;
    }

    let tags: Vec<_> = RegistryKey::NETWORK_KEYS
        .iter()
        .copied()
        .filter(|key| {
            get_registry_key_tags(CURRENT_MC_VERSION, *key)
                .is_some_and(|registry_tags| !registry_tags.is_empty())
        })
        .collect();
    let mut payload = Vec::new();
    write_varint(
        i32::try_from(tags.len())
            .map_err(|_| invalid_data("tag registry count exceeds VarInt range"))?,
        &mut payload,
    );
    for key in tags {
        let registry_tags = get_registry_key_tags(CURRENT_MC_VERSION, key)
            .ok_or_else(|| invalid_data("tag registry data disappeared during serialization"))?;
        write_string(
            format!("minecraft:{}", key.identifier_string()).as_bytes(),
            &mut payload,
        )?;
        write_varint(
            i32::try_from(registry_tags.len())
                .map_err(|_| invalid_data("tag count exceeds VarInt range"))?,
            &mut payload,
        );
        for (tag_name, tag) in registry_tags.entries() {
            write_string(tag_name.as_bytes(), &mut payload)?;
            write_varint(
                i32::try_from(tag.1.len())
                    .map_err(|_| invalid_data("tag value count exceeds VarInt range"))?,
                &mut payload,
            );
            for value in tag.1 {
                let remapped_id = match key {
                    RegistryKey::Item => remap_item_id_for_version(*value, CURRENT_MC_VERSION),
                    RegistryKey::EntityType => {
                        remap_entity_id_for_version(*value, CURRENT_MC_VERSION)
                    }
                    _ => *value,
                };
                write_varint(i32::from(remapped_id), &mut payload);
            }
        }
    }
    write_clientbound_packet(stream, &packet::clientbound::config::UPDATE_TAGS, &payload)?;
    write_clientbound_packet(
        stream,
        &packet::clientbound::config::FINISH_CONFIGURATION,
        &[],
    )
}

fn enter_play(stream: &mut TcpStream) -> io::Result<()> {
    let dimension_id = registry_entry_id("minecraft:dimension_type", "minecraft:overworld")?;
    let mut login = Vec::new();
    login.extend_from_slice(&1i32.to_be_bytes());
    login.push(0);
    write_varint(1, &mut login);
    write_string(b"minecraft:overworld", &mut login)?;
    write_varint(20, &mut login);
    write_varint(8, &mut login);
    write_varint(8, &mut login);
    login.push(0);
    login.push(1);
    login.push(0);
    write_varint(dimension_id, &mut login);
    write_string(b"minecraft:overworld", &mut login)?;
    login.extend_from_slice(&0i64.to_be_bytes());
    login.push(0);
    login.push(0xff);
    login.push(0);
    login.push(0);
    login.push(0);
    write_varint(0, &mut login);
    write_varint(63, &mut login);
    login.push(0);
    login.push(0);
    write_clientbound_packet(stream, &packet::clientbound::play::LOGIN, &login)?;

    let mut spawn_position = Vec::new();
    write_string(b"minecraft:overworld", &mut spawn_position)?;
    spawn_position.extend_from_slice(&pack_block_position(SPAWN_X, SPAWN_Y, SPAWN_Z).to_be_bytes());
    spawn_position.extend_from_slice(&0.0f32.to_be_bytes());
    spawn_position.extend_from_slice(&0.0f32.to_be_bytes());
    write_clientbound_packet(
        stream,
        &packet::clientbound::play::SET_DEFAULT_SPAWN_POSITION,
        &spawn_position,
    )?;

    let mut center_chunk = Vec::new();
    write_varint(0, &mut center_chunk);
    write_varint(0, &mut center_chunk);
    write_clientbound_packet(
        stream,
        &packet::clientbound::play::SET_CHUNK_CACHE_CENTER,
        &center_chunk,
    )?;
    write_clientbound_packet(stream, &packet::clientbound::play::CHUNK_BATCH_START, &[])?;
    write_clientbound_packet(
        stream,
        &packet::clientbound::play::LEVEL_CHUNK_WITH_LIGHT,
        &initial_chunk_data()?,
    )?;
    let mut chunk_batch_end = Vec::new();
    write_varint(1, &mut chunk_batch_end);
    write_clientbound_packet(
        stream,
        &packet::clientbound::play::CHUNK_BATCH_FINISHED,
        &chunk_batch_end,
    )?;

    let mut player_position = Vec::new();
    write_varint(TELEPORT_ID, &mut player_position);
    for coordinate in [
        f64::from(SPAWN_X) + 0.5,
        f64::from(SPAWN_Y),
        f64::from(SPAWN_Z) + 0.5,
        0.0,
        0.0,
        0.0,
    ] {
        player_position.extend_from_slice(&coordinate.to_be_bytes());
    }
    player_position.extend_from_slice(&0.0f32.to_be_bytes());
    player_position.extend_from_slice(&0.0f32.to_be_bytes());
    player_position.extend_from_slice(&0i32.to_be_bytes());
    write_clientbound_packet(
        stream,
        &packet::clientbound::play::PLAYER_POSITION,
        &player_position,
    )?;
    handle_play(stream)
}

fn initial_chunk_data() -> io::Result<Vec<u8>> {
    const HEIGHTMAP_LONGS: usize = 37;
    let heightmap_value = (PLATFORM_BLOCK_Y + 1 - OVERWORLD_MIN_Y) as u64;
    let plains_biome_id = registry_entry_id("minecraft:worldgen/biome", "minecraft:plains")?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&0i32.to_be_bytes());
    payload.extend_from_slice(&0i32.to_be_bytes());

    write_varint(3, &mut payload);
    for heightmap_id in [1, 4, 5] {
        write_varint(heightmap_id, &mut payload);
        write_varint(HEIGHTMAP_LONGS as i32, &mut payload);
        for word_index in 0..HEIGHTMAP_LONGS {
            let mut packed = 0u64;
            for value_index in 0..7 {
                let height_index = word_index * 7 + value_index;
                if height_index < 256 {
                    packed |= heightmap_value << (value_index * 9);
                }
            }
            payload.extend_from_slice(&(packed as i64).to_be_bytes());
        }
    }

    let mut sections = Vec::new();
    for section_index in 0..CHUNK_SECTION_COUNT {
        let has_platform = section_index == PLATFORM_SECTION_INDEX;
        sections.extend_from_slice(&(if has_platform { 256i16 } else { 0i16 }).to_be_bytes());
        sections.extend_from_slice(&0i16.to_be_bytes());

        if has_platform {
            sections.push(4);
            write_varint(2, &mut sections);
            write_varint(0, &mut sections);
            write_varint(STONE_BLOCK_STATE_ID, &mut sections);
            for _ in 0..16 {
                sections.extend_from_slice(&0x1111_1111_1111_1111i64.to_be_bytes());
            }
        } else {
            sections.push(0);
            write_varint(0, &mut sections);
        }

        sections.push(0);
        write_varint(plains_biome_id, &mut sections);
    }
    write_varint(
        i32::try_from(sections.len())
            .map_err(|_| invalid_data("chunk section data exceeds VarInt range"))?,
        &mut payload,
    );
    payload.extend_from_slice(&sections);
    write_varint(0, &mut payload);

    let section_mask = ((1u64 << (CHUNK_SECTION_COUNT + 1)) - 1) & !1;
    let empty_edge_mask = 1u64 | (1u64 << (CHUNK_SECTION_COUNT + 1));
    write_bitset(section_mask, &mut payload);
    write_bitset(0, &mut payload);
    write_bitset(empty_edge_mask, &mut payload);
    write_bitset((1u64 << (CHUNK_SECTION_COUNT + 2)) - 1, &mut payload);

    write_varint(CHUNK_SECTION_COUNT as i32, &mut payload);
    for _ in 0..CHUNK_SECTION_COUNT {
        write_varint(2048, &mut payload);
        payload.extend_from_slice(&[0xff; 2048]);
    }
    write_varint(0, &mut payload);

    Ok(payload)
}

fn registry_entry_id(registry_id: &str, entry_id: &str) -> io::Result<i32> {
    let registries = Registry::get_synced(CURRENT_MC_VERSION);
    let registry = registries
        .iter()
        .find(|registry| registry.registry_id == registry_id)
        .ok_or_else(|| invalid_data("required 26.2 registry is missing"))?;
    let id = registry
        .registry_entries
        .iter()
        .position(|entry| entry.entry_id == entry_id)
        .ok_or_else(|| invalid_data("required 26.2 registry entry is missing"))?;
    i32::try_from(id).map_err(|_| invalid_data("registry entry ID exceeds VarInt range"))
}

fn pack_block_position(x: i32, y: i32, z: i32) -> i64 {
    (i64::from(x & 0x3ff_ffff) << 38) | (i64::from(z & 0x3ff_ffff) << 12) | i64::from(y & 0xfff)
}

fn write_bitset(value: u64, output: &mut Vec<u8>) {
    write_varint(1, output);
    output.extend_from_slice(&(value as i64).to_be_bytes());
}

fn handle_play(stream: &mut TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let teleport_packet_id = packet_id(&packet::serverbound::play::ACCEPT_TELEPORTATION)?;
    let keepalive_packet_id = packet_id(&packet::serverbound::play::KEEP_ALIVE)?;
    let clientbound_keepalive_id = packet_id(&packet::clientbound::play::KEEP_ALIVE)?;
    let mut keepalive_stream = stream.try_clone()?;
    let keepalive_running = Arc::new(AtomicBool::new(true));
    let worker_running = Arc::clone(&keepalive_running);
    let pending_keepalive = Arc::new(AtomicI64::new(i64::MIN));
    let worker_pending_keepalive = Arc::clone(&pending_keepalive);
    let keepalive_thread = thread::spawn(move || {
        let mut keepalive_id = 0i64;
        while worker_running.load(Ordering::Relaxed) {
            thread::park_timeout(Duration::from_secs(15));
            if !worker_running.load(Ordering::Relaxed) {
                break;
            }
            keepalive_id = keepalive_id.wrapping_add(1);
            worker_pending_keepalive.store(keepalive_id, Ordering::Release);
            let mut payload = Vec::with_capacity(9);
            write_varint(clientbound_keepalive_id, &mut payload);
            payload.extend_from_slice(&keepalive_id.to_be_bytes());
            if let Err(error) = write_frame(&mut keepalive_stream, &payload) {
                eprintln!("failed to send play keepalive: {error}");
                break;
            }
        }
    });

    let mut reported_unhandled = HashSet::new();
    let result = loop {
        let frame = match read_frame(stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(error) => break Err(error),
        };
        let mut cursor = 0;
        let received_id = match read_varint(&frame, &mut cursor) {
            Ok(id) => id,
            Err(error) => break Err(error),
        };

        if received_id == teleport_packet_id {
            let confirmed_id = match read_varint(&frame, &mut cursor) {
                Ok(id) => id,
                Err(error) => break Err(error),
            };
            if confirmed_id != TELEPORT_ID || cursor != frame.len() {
                break Err(invalid_data("invalid teleport confirmation"));
            }
        } else if received_id == keepalive_packet_id {
            if frame.len() - cursor != 8 {
                break Err(invalid_data(
                    "keepalive response must contain exactly 8 bytes",
                ));
            }
            let mut response_bytes = [0; 8];
            response_bytes.copy_from_slice(&frame[cursor..]);
            let response_id = i64::from_be_bytes(response_bytes);
            let expected_id = pending_keepalive.load(Ordering::Acquire);
            if response_id != expected_id {
                break Err(invalid_data(
                    "keepalive response did not match the outstanding request",
                ));
            }
            pending_keepalive.store(i64::MIN, Ordering::Release);
        } else if reported_unhandled.insert(received_id) {
            eprintln!("ignoring unsupported play packet id {received_id}");
        }
    };

    keepalive_running.store(false, Ordering::Relaxed);
    keepalive_thread.thread().unpark();
    if keepalive_thread.join().is_err() {
        eprintln!("play keepalive worker panicked");
    }
    result
}

fn read_client_information(frame: &[u8], cursor: &mut usize) -> io::Result<()> {
    read_bounded_string(frame, cursor, 16)?;
    read_exact(frame, cursor, 1)?;
    let chat_mode = read_varint(frame, cursor)?;
    if !(0..=2).contains(&chat_mode) {
        return Err(invalid_data("invalid client chat mode"));
    }
    read_bool(frame, cursor)?;
    read_exact(frame, cursor, 1)?;
    let main_hand = read_varint(frame, cursor)?;
    if !(0..=1).contains(&main_hand) {
        return Err(invalid_data("invalid client main-hand setting"));
    }
    read_bool(frame, cursor)?;
    read_bool(frame, cursor)?;
    if *cursor != frame.len() {
        return Err(invalid_data(
            "unexpected trailing bytes in client information",
        ));
    }
    Ok(())
}

fn read_selected_known_packs(frame: &[u8], cursor: &mut usize) -> io::Result<()> {
    let count = read_varint(frame, cursor)?;
    if !(0..=64).contains(&count) {
        return Err(invalid_data("known-packs count is out of bounds"));
    }
    for _ in 0..count {
        read_bounded_string(frame, cursor, 255)?;
        read_bounded_string(frame, cursor, 255)?;
        read_bounded_string(frame, cursor, 255)?;
    }
    if *cursor != frame.len() {
        return Err(invalid_data(
            "unexpected trailing bytes in known-packs selection",
        ));
    }
    Ok(())
}

fn read_bool(bytes: &[u8], cursor: &mut usize) -> io::Result<bool> {
    match read_exact(bytes, cursor, 1)?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_data("boolean field must be 0 or 1")),
    }
}

fn packet_id(packet: &PacketId) -> io::Result<i32> {
    let id = packet.v26_2;
    if id < 0 {
        return Err(invalid_data(
            "required packet is unavailable in Minecraft 26.2",
        ));
    }
    Ok(id)
}

fn write_clientbound_packet(
    stream: &mut TcpStream,
    packet: &PacketId,
    payload: &[u8],
) -> io::Result<()> {
    write_raw_packet(stream, packet_id(packet)?, payload)
}

fn write_raw_packet(stream: &mut TcpStream, packet_id: i32, payload: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(payload.len() + 5);
    write_varint(packet_id, &mut body);
    body.extend_from_slice(payload);
    write_frame(stream, &body)
}

fn write_login_disconnect(stream: &mut TcpStream, message: &str) -> io::Result<()> {
    write_disconnect(stream, LOGIN_DISCONNECT_PACKET, message)
}

fn write_disconnect(stream: &mut TcpStream, packet_id: i32, message: &str) -> io::Result<()> {
    let component = serde_json::json!({ "text": message }).to_string();
    write_packet(stream, packet_id, component.as_bytes())
}

fn valid_username(username: &str) -> bool {
    (3..=16).contains(&username.len())
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn status_response() -> String {
    serde_json::json!({
        "version": {
            "name": MINECRAFT_VERSION,
            "protocol": PROTOCOL_VERSION
        },
        "players": {
            "max": 20,
            "online": 0
        },
        "description": {
            "text": "Rubber experimental 26.2 server"
        }
    })
    .to_string()
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let frame_length = read_varint_stream(stream)?;
    if frame_length < 0 || frame_length as usize > MAX_PACKET_SIZE {
        return Err(invalid_data("packet frame exceeds the 1 MiB limit"));
    }
    let mut frame = vec![0; frame_length as usize];
    stream.read_exact(&mut frame)?;
    Ok(frame)
}

fn write_packet(stream: &mut TcpStream, packet_id: i32, payload: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(payload.len() + 5);
    write_varint(packet_id, &mut body);
    write_string(payload, &mut body)?;
    write_frame(stream, &body)
}

fn write_frame(stream: &mut TcpStream, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_PACKET_SIZE {
        return Err(invalid_data("outgoing packet exceeds the 1 MiB limit"));
    }
    let mut header = Vec::with_capacity(5);
    write_varint(body.len() as i32, &mut header);
    stream.write_all(&header)?;
    stream.write_all(body)
}

fn read_varint_stream(stream: &mut TcpStream) -> io::Result<i32> {
    let mut value = 0u32;
    for byte_index in 0..5 {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte_index == 4 && byte[0] & 0xf0 != 0 {
            return Err(invalid_data("VarInt exceeds 32 bits"));
        }
        value |= u32::from(byte[0] & 0x7f) << (byte_index * 7);
        if byte[0] & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(invalid_data("VarInt exceeds 5 bytes"))
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> io::Result<i32> {
    let mut value = 0u32;
    for byte_index in 0..5 {
        let byte = *bytes
            .get(*cursor)
            .ok_or_else(|| invalid_data("truncated VarInt"))?;
        *cursor += 1;
        if byte_index == 4 && byte & 0xf0 != 0 {
            return Err(invalid_data("VarInt exceeds 32 bits"));
        }
        value |= u32::from(byte & 0x7f) << (byte_index * 7);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(invalid_data("VarInt exceeds 5 bytes"))
}

fn write_varint(value: i32, output: &mut Vec<u8>) {
    let mut value = value as u32;
    loop {
        if value & !0x7f == 0 {
            output.push(value as u8);
            return;
        }
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> io::Result<String> {
    read_bounded_string(bytes, cursor, 255)
}

fn read_bounded_string(bytes: &[u8], cursor: &mut usize, max_length: usize) -> io::Result<String> {
    let length = read_varint(bytes, cursor)?;
    if length < 0 || length as usize > max_length {
        return Err(invalid_data("protocol string exceeds its byte limit"));
    }
    let content = read_exact(bytes, cursor, length as usize)?;
    String::from_utf8(content.to_vec()).map_err(|_| invalid_data("protocol string is not UTF-8"))
}

fn write_string(value: &[u8], output: &mut Vec<u8>) -> io::Result<()> {
    let length = i32::try_from(value.len()).map_err(|_| invalid_data("string is too long"))?;
    write_varint(length, output);
    output.extend_from_slice(value);
    Ok(())
}

fn read_exact<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> io::Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| invalid_data("packet length overflow"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or_else(|| invalid_data("truncated packet"))?;
    *cursor = end;
    Ok(value)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener};

    fn spawn_test_server() -> (TcpStream, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_connection(stream).unwrap();
        });
        (TcpStream::connect(address).unwrap(), server)
    }

    fn send_handshake(client: &mut TcpStream, next_state: i32, protocol: i32) {
        let mut handshake = Vec::new();
        write_varint(0, &mut handshake);
        write_varint(protocol, &mut handshake);
        write_string(b"localhost", &mut handshake).unwrap();
        handshake.extend_from_slice(&25565u16.to_be_bytes());
        write_varint(next_state, &mut handshake);
        write_frame(client, &handshake).unwrap();
    }

    #[test]
    fn status_ping_returns_supported_version_and_echoes_ping() {
        let (mut client, server) = spawn_test_server();
        send_handshake(&mut client, HANDSHAKE_STATUS, PROTOCOL_VERSION);

        let mut status_request = Vec::new();
        write_varint(0, &mut status_request);
        write_frame(&mut client, &status_request).unwrap();

        let status_frame = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(read_varint(&status_frame, &mut cursor).unwrap(), 0);
        let json_length = read_varint(&status_frame, &mut cursor).unwrap() as usize;
        let json = std::str::from_utf8(&status_frame[cursor..cursor + json_length]).unwrap();
        let status: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(status["version"]["name"], "26.2");
        assert_eq!(status["version"]["protocol"], 776);

        let ping_value = 0x0102_0304_0506_0708i64;
        let mut ping = Vec::new();
        write_varint(1, &mut ping);
        ping.extend_from_slice(&ping_value.to_be_bytes());
        write_frame(&mut client, &ping).unwrap();

        let pong_frame = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(read_varint(&pong_frame, &mut cursor).unwrap(), 1);
        assert_eq!(
            i64::from_be_bytes(pong_frame[cursor..].try_into().unwrap()),
            ping_value
        );
        server.join().unwrap();
    }

    #[test]
    fn offline_login_syncs_registries_and_enters_the_initial_play_world() {
        let (mut client, server) = spawn_test_server();
        send_handshake(&mut client, HANDSHAKE_LOGIN, PROTOCOL_VERSION);

        let mut login_start = Vec::new();
        write_varint(
            packet_id(&packet::serverbound::login::LOGIN_START).unwrap(),
            &mut login_start,
        );
        write_string(b"RubberUser", &mut login_start).unwrap();
        login_start.extend_from_slice(uuid::Uuid::nil().as_bytes());
        write_frame(&mut client, &login_start).unwrap();

        let login_success = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&login_success, &mut cursor).unwrap(),
            packet_id(&packet::clientbound::login::LOGIN_SUCCESS).unwrap()
        );
        assert_eq!(
            login_success[cursor..cursor + 16],
            *uuid::Uuid::nil().as_bytes()
        );
        cursor += 16;
        assert_eq!(
            read_bounded_string(&login_success, &mut cursor, 16).unwrap(),
            "RubberUser"
        );
        assert_eq!(read_varint(&login_success, &mut cursor).unwrap(), 0);
        assert_eq!(login_success.len() - cursor, 16);

        let mut acknowledgement = Vec::new();
        write_varint(
            packet_id(&packet::serverbound::login::LOGIN_ACKNOWLEDGED).unwrap(),
            &mut acknowledgement,
        );
        write_frame(&mut client, &acknowledgement).unwrap();

        let known_packs = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&known_packs, &mut cursor).unwrap(),
            packet_id(&packet::clientbound::config::SELECT_KNOWN_PACKS).unwrap()
        );
        assert_eq!(read_varint(&known_packs, &mut cursor).unwrap(), 1);
        assert_eq!(
            read_bounded_string(&known_packs, &mut cursor, 255).unwrap(),
            "minecraft"
        );
        assert_eq!(
            read_bounded_string(&known_packs, &mut cursor, 255).unwrap(),
            "core"
        );
        assert_eq!(
            read_bounded_string(&known_packs, &mut cursor, 255).unwrap(),
            MINECRAFT_VERSION
        );
        assert_eq!(cursor, known_packs.len());

        let mut client_information = Vec::new();
        write_string(b"en_us", &mut client_information).unwrap();
        client_information.push(8);
        write_varint(0, &mut client_information);
        client_information.push(1);
        client_information.push(0x7f);
        write_varint(1, &mut client_information);
        client_information.push(0);
        client_information.push(1);
        send_test_packet(
            &mut client,
            packet_id(&packet::serverbound::config::CLIENT_INFORMATION).unwrap(),
            &client_information,
        );
        let mut selected_packs = Vec::new();
        write_varint(0, &mut selected_packs);
        send_test_packet(
            &mut client,
            packet_id(&packet::serverbound::config::SELECT_KNOWN_PACKS).unwrap(),
            &selected_packs,
        );

        let mut registry_packets = 0;
        let mut saw_tags = false;
        loop {
            let frame = read_test_frame(&mut client);
            let mut cursor = 0;
            let received_id = read_varint(&frame, &mut cursor).unwrap();
            if received_id == packet_id(&packet::clientbound::config::REGISTRY_DATA).unwrap() {
                assert!(!read_string(&frame, &mut cursor).unwrap().is_empty());
                assert!(read_varint(&frame, &mut cursor).unwrap() > 0);
                registry_packets += 1;
            } else if received_id == packet::clientbound::config::UPDATE_ENABLED_FEATURES.v26_2 {
                assert_eq!(read_varint(&frame, &mut cursor).unwrap(), 1);
                assert_eq!(
                    read_string(&frame, &mut cursor).unwrap(),
                    "minecraft:vanilla"
                );
                assert_eq!(cursor, frame.len());
            } else if received_id == packet_id(&packet::clientbound::config::UPDATE_TAGS).unwrap() {
                assert!(read_varint(&frame, &mut cursor).unwrap() > 0);
                saw_tags = true;
            } else if received_id
                == packet_id(&packet::clientbound::config::FINISH_CONFIGURATION).unwrap()
            {
                assert_eq!(cursor, frame.len());
                break;
            } else {
                panic!("unexpected configuration packet id {received_id}");
            }
        }
        assert_eq!(
            registry_packets,
            Registry::get_synced(CURRENT_MC_VERSION).len()
        );
        assert!(saw_tags);

        send_test_packet(
            &mut client,
            packet_id(&packet::serverbound::config::FINISH_CONFIGURATION).unwrap(),
            &[],
        );

        let login = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&login, &mut cursor).unwrap(),
            packet_id(&packet::clientbound::play::LOGIN).unwrap()
        );
        assert_eq!(
            i32::from_be_bytes(
                read_exact(&login, &mut cursor, 4)
                    .unwrap()
                    .try_into()
                    .unwrap()
            ),
            1
        );
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 1);
        assert_eq!(
            read_string(&login, &mut cursor).unwrap(),
            "minecraft:overworld"
        );
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 20);
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 8);
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 8);
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert!(read_bool(&login, &mut cursor).unwrap());
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert!(read_varint(&login, &mut cursor).unwrap() >= 0);
        assert_eq!(
            read_string(&login, &mut cursor).unwrap(),
            "minecraft:overworld"
        );
        assert_eq!(read_exact(&login, &mut cursor, 8).unwrap(), &[0; 8]);
        assert_eq!(read_exact(&login, &mut cursor, 1).unwrap(), &[0]);
        assert_eq!(read_exact(&login, &mut cursor, 1).unwrap(), &[0xff]);
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 0);
        assert_eq!(read_varint(&login, &mut cursor).unwrap(), 63);
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert!(!read_bool(&login, &mut cursor).unwrap());
        assert_eq!(cursor, login.len());

        let mut saw_chunk = false;
        let mut saw_position = false;
        for _ in 0..8 {
            let frame = read_test_frame(&mut client);
            let mut cursor = 0;
            let received_id = read_varint(&frame, &mut cursor).unwrap();
            if received_id == packet_id(&packet::clientbound::play::LEVEL_CHUNK_WITH_LIGHT).unwrap()
            {
                assert_eq!(read_exact(&frame, &mut cursor, 8).unwrap(), &[0; 8]);
                assert!(frame.len() > cursor + 100);
                saw_chunk = true;
            } else if received_id == packet_id(&packet::clientbound::play::PLAYER_POSITION).unwrap()
            {
                assert_eq!(read_varint(&frame, &mut cursor).unwrap(), TELEPORT_ID);
                saw_position = true;
            }
            if saw_chunk && saw_position {
                break;
            }
        }
        assert!(saw_chunk);
        assert!(saw_position);

        let mut teleport_ack = Vec::new();
        write_varint(TELEPORT_ID, &mut teleport_ack);
        send_test_packet(
            &mut client,
            packet_id(&packet::serverbound::play::ACCEPT_TELEPORTATION).unwrap(),
            &teleport_ack,
        );
        client.shutdown(std::net::Shutdown::Both).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn initial_chunk_contains_a_platform_and_valid_26_2_light_sections() {
        let chunk = initial_chunk_data().unwrap();
        let mut cursor = 0;
        assert_eq!(read_exact(&chunk, &mut cursor, 8).unwrap(), &[0; 8]);
        let heightmap_value = (PLATFORM_BLOCK_Y + 1 - OVERWORLD_MIN_Y) as u64;
        let plains_id = registry_entry_id("minecraft:worldgen/biome", "minecraft:plains").unwrap();
        assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 3);
        for expected_id in [1, 4, 5] {
            assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), expected_id);
            assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 37);
            for word_index in 0..37 {
                let mut expected_heightmap = 0u64;
                for value_index in 0..7 {
                    if word_index * 7 + value_index < 256 {
                        expected_heightmap |= heightmap_value << (value_index * 9);
                    }
                }
                assert_eq!(
                    i64::from_be_bytes(
                        read_exact(&chunk, &mut cursor, 8)
                            .unwrap()
                            .try_into()
                            .unwrap()
                    ) as u64,
                    expected_heightmap
                );
            }
        }
        let section_data_length = read_varint(&chunk, &mut cursor).unwrap() as usize;
        assert!(section_data_length > 24 * 5);
        let section_data = read_exact(&chunk, &mut cursor, section_data_length).unwrap();
        let mut section_cursor = 0;
        for section_index in 0..24 {
            let block_count = i16::from_be_bytes(
                read_exact(section_data, &mut section_cursor, 2)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(
                i16::from_be_bytes(
                    read_exact(section_data, &mut section_cursor, 2)
                        .unwrap()
                        .try_into()
                        .unwrap()
                ),
                0
            );
            if section_index == 4 {
                assert_eq!(block_count, 256);
                assert_eq!(
                    read_exact(section_data, &mut section_cursor, 1).unwrap(),
                    &[4]
                );
                assert_eq!(read_varint(section_data, &mut section_cursor).unwrap(), 2);
                assert_eq!(read_varint(section_data, &mut section_cursor).unwrap(), 0);
                assert_eq!(read_varint(section_data, &mut section_cursor).unwrap(), 1);
                for _ in 0..16 {
                    assert_eq!(
                        read_exact(section_data, &mut section_cursor, 8).unwrap(),
                        &0x1111_1111_1111_1111i64.to_be_bytes()
                    );
                }
            } else {
                assert_eq!(block_count, 0);
                assert_eq!(
                    read_exact(section_data, &mut section_cursor, 1).unwrap(),
                    &[0]
                );
                assert_eq!(read_varint(section_data, &mut section_cursor).unwrap(), 0);
            }
            assert_eq!(
                read_exact(section_data, &mut section_cursor, 1).unwrap(),
                &[0]
            );
            assert_eq!(
                read_varint(section_data, &mut section_cursor).unwrap(),
                plains_id
            );
        }
        assert_eq!(section_cursor, section_data.len());

        assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 0);
        for expected_mask in [
            ((1u64 << 25) - 1) & !1,
            0,
            1 | (1u64 << 25),
            (1u64 << 26) - 1,
        ] {
            assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 1);
            assert_eq!(
                i64::from_be_bytes(
                    read_exact(&chunk, &mut cursor, 8)
                        .unwrap()
                        .try_into()
                        .unwrap()
                ) as u64,
                expected_mask
            );
        }
        assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 24);
        for _ in 0..24 {
            assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 2048);
            read_exact(&chunk, &mut cursor, 2048).unwrap();
        }
        assert_eq!(read_varint(&chunk, &mut cursor).unwrap(), 0);
        assert_eq!(cursor, chunk.len());
    }

    #[test]
    fn login_rejects_wrong_protocol_before_reading_login_start() {
        let (mut client, server) = spawn_test_server();
        send_handshake(&mut client, HANDSHAKE_LOGIN, PROTOCOL_VERSION - 1);
        let disconnect = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&disconnect, &mut cursor).unwrap(),
            LOGIN_DISCONNECT_PACKET
        );
        let reason_length = read_varint(&disconnect, &mut cursor).unwrap() as usize;
        let reason: serde_json::Value =
            serde_json::from_slice(&disconnect[cursor..cursor + reason_length]).unwrap();
        assert!(reason["text"].as_str().unwrap().contains("protocol 776"));
        server.join().unwrap();
    }

    #[test]
    fn varint_round_trips_protocol_values() {
        for value in [0, 1, 127, 128, 776, i32::MAX, -1] {
            let mut encoded = Vec::new();
            write_varint(value, &mut encoded);
            let mut cursor = 0;
            assert_eq!(read_varint(&encoded, &mut cursor).unwrap(), value);
            assert_eq!(cursor, encoded.len());
        }
        assert!(read_varint(&[0x80], &mut 0).is_err());
        assert!(read_varint(&[0x80, 0x80, 0x80, 0x80, 0x80], &mut 0).is_err());
        assert!(read_varint(&[0xff, 0xff, 0xff, 0xff, 0x10], &mut 0).is_err());
    }

    fn send_test_packet(stream: &mut TcpStream, packet_id: i32, payload: &[u8]) {
        let mut body = Vec::with_capacity(payload.len() + 5);
        write_varint(packet_id, &mut body);
        body.extend_from_slice(payload);
        write_frame(stream, &body).unwrap();
    }

    fn read_test_frame(stream: &mut TcpStream) -> Vec<u8> {
        let mut first_byte = [0];
        stream.read_exact(&mut first_byte).unwrap();
        let mut encoded_length = vec![first_byte[0]];
        while encoded_length.last().unwrap() & 0x80 != 0 {
            stream.read_exact(&mut first_byte).unwrap();
            encoded_length.push(first_byte[0]);
        }
        let mut cursor = 0;
        let length = read_varint(&encoded_length, &mut cursor).unwrap() as usize;
        let mut frame = vec![0; length];
        stream.read_exact(&mut frame).unwrap();
        frame
    }
}
