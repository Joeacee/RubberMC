use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};

const MINECRAFT_VERSION: &str = "26.2";
const PROTOCOL_VERSION: i32 = 776;
const MAX_PACKET_SIZE: usize = 1024 * 1024;
const HANDSHAKE_STATUS: i32 = 1;
const HANDSHAKE_LOGIN: i32 = 2;
const LOGIN_START_PACKET: i32 = 0;
const LOGIN_SUCCESS_PACKET: i32 = 2;
const LOGIN_ACKNOWLEDGED_PACKET: i32 = 3;
const LOGIN_DISCONNECT_PACKET: i32 = 0;
const CONFIGURATION_DISCONNECT_PACKET: i32 = 2;

pub fn serve(bind_address: &str) -> io::Result<()> {
    let listener = TcpListener::bind(bind_address)?;
    println!(
        "Rubber 26.2 status server listening on {}",
        listener.local_addr()?
    );
    println!("Server-list status and offline login are enabled; gameplay is not implemented.");
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
    if read_varint(&login_start, &mut cursor)? != LOGIN_START_PACKET {
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
    write_varint(LOGIN_SUCCESS_PACKET, &mut login_success);
    login_success.extend_from_slice(uuid_bytes);
    write_string(username.as_bytes(), &mut login_success)?;
    write_varint(0, &mut login_success);
    login_success.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    write_frame(stream, &login_success)?;

    let acknowledgement = read_frame(stream)?;
    let mut cursor = 0;
    if read_varint(&acknowledgement, &mut cursor)? != LOGIN_ACKNOWLEDGED_PACKET
        || cursor != acknowledgement.len()
    {
        return write_login_disconnect(stream, "Expected login acknowledgement.");
    }

    eprintln!(
        "Offline login completed for '{username}'; rejecting before configuration because 26.2 registries and world entry are not implemented."
    );
    write_configuration_disconnect(
        stream,
        "Rubber accepted the offline login, but world entry is not implemented yet.",
    )
}

fn write_login_disconnect(stream: &mut TcpStream, message: &str) -> io::Result<()> {
    write_disconnect(stream, LOGIN_DISCONNECT_PACKET, message)
}

fn write_configuration_disconnect(stream: &mut TcpStream, message: &str) -> io::Result<()> {
    write_disconnect(stream, CONFIGURATION_DISCONNECT_PACKET, message)
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
            "text": "Rubber development server (status-only)"
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
    String::from_utf8(content.to_vec()).map_err(|_| invalid_data("server address is not UTF-8"))
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
    fn offline_login_succeeds_then_disconnects_with_configuration_limit() {
        let (mut client, server) = spawn_test_server();
        send_handshake(&mut client, HANDSHAKE_LOGIN, PROTOCOL_VERSION);

        let mut login_start = Vec::new();
        write_varint(LOGIN_START_PACKET, &mut login_start);
        write_string(b"RubberUser", &mut login_start).unwrap();
        login_start.extend_from_slice(uuid::Uuid::nil().as_bytes());
        write_frame(&mut client, &login_start).unwrap();

        let login_success = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&login_success, &mut cursor).unwrap(),
            LOGIN_SUCCESS_PACKET
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
        write_varint(LOGIN_ACKNOWLEDGED_PACKET, &mut acknowledgement);
        write_frame(&mut client, &acknowledgement).unwrap();
        let disconnect = read_test_frame(&mut client);
        let mut cursor = 0;
        assert_eq!(
            read_varint(&disconnect, &mut cursor).unwrap(),
            CONFIGURATION_DISCONNECT_PACKET
        );
        let reason_length = read_varint(&disconnect, &mut cursor).unwrap() as usize;
        let reason: serde_json::Value =
            serde_json::from_slice(&disconnect[cursor..cursor + reason_length]).unwrap();
        assert!(reason["text"]
            .as_str()
            .unwrap()
            .contains("world entry is not implemented"));
        server.join().unwrap();
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
