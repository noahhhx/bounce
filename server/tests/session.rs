//! Runs the server binary with a test tone and plays a client by hand over
//! the wire: connect, apply desired state, get routed, hear audio, leave.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use bounce_common::audio::Codec;
use bounce_common::ids::{BootId, ClientId, Version};
use bounce_common::protocol::{
    ClientMessage, Hello, PlayerState, ReportedState, ServerMessage, PROTOCOL_VERSION,
};
use bounce_common::update::{Target, UpdateState};
use bounce_common::wire;
use bounce_server::config::READY_PREFIX;

const CLIENT: &str = "b0c00000000000000000000000000001";

struct Server {
    child: Child,
    session: String,
    http: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server() -> Server {
    let data_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("session-test");
    let mut child = Command::new(env!("CARGO_BIN_EXE_bounce-server"))
        .args([
            "--listen",
            "127.0.0.1:0",
            "--http",
            "127.0.0.1:0",
            "--no-mdns",
        ])
        .args(["--source", "test-tone", "--data-dir"])
        .arg(data_dir)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addrs = line.trim().strip_prefix(READY_PREFIX).unwrap();
    let addr = |key: &str| {
        addrs
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key))
            .unwrap()
            .to_owned()
    };
    Server {
        session: addr("session="),
        http: addr("http="),
        child,
    }
}

async fn send(stream: &mut TcpStream, message: &ClientMessage) {
    stream
        .write_all(&wire::encode(message).unwrap())
        .await
        .unwrap();
}

async fn recv(stream: &mut TcpStream) -> ServerMessage {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).await.unwrap();
    let mut body = vec![0; u32::from_le_bytes(prefix) as usize];
    stream.read_exact(&mut body).await.unwrap();
    wire::decode(&body).unwrap()
}

async fn http(server: &Server, method: &str, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(&server.http).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response.split_once("\r\n\r\n").unwrap().1.to_owned()
}

async fn admin(server: &Server, request: &str) {
    http(server, "POST", "/api/admin", request).await;
}

/// The client's entry in `/api/state`, once `ready` holds for it.
async fn client_state(server: &Server, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..50 {
        let state: Value =
            serde_json::from_str(&http(server, "GET", "/api/state", "").await).unwrap();
        let client = state["clients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == CLIENT)
            .cloned();
        if let Some(client) = client.filter(&ready) {
            println!("state: {client}");
            return client;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("client never reached the expected state");
}

fn hello() -> ClientMessage {
    ClientMessage::Hello(Hello {
        protocol: PROTOCOL_VERSION,
        client: ClientId::parse(CLIENT).unwrap(),
        boot: BootId(1),
        version: Version {
            major: 0,
            minor: 1,
            patch: 0,
        },
        target: Target::X86_64,
        hostname: "smoke".to_owned(),
        outputs: vec![],
        codecs: vec![Codec::PcmS16Le],
    })
}

#[tokio::test]
async fn a_client_connects_applies_its_state_hears_audio_and_goes_offline() {
    let server = start_server();
    let mut stream = TcpStream::connect(&server.session).await.unwrap();
    send(&mut stream, &hello()).await;

    let ServerMessage::Desired(desired) = recv(&mut stream).await else {
        panic!("the first message after Hello must be Desired");
    };
    let reported = ReportedState {
        applied: desired.revision,
        player: PlayerState::Idle,
        update: UpdateState::UpToDate,
    };
    send(&mut stream, &ClientMessage::Reported(reported)).await;
    client_state(&server, |c| {
        c["online"] == true && c["reported"]["applied"] == c["desired"]["revision"]
    })
    .await;

    admin(&server, r#"{"UpsertZone":{"zone":1,"name":"K"}}"#).await;
    admin(
        &server,
        &format!(r#"{{"MoveClient":{{"client":"{CLIENT}","zone":1}}}}"#),
    )
    .await;
    admin(&server, r#"{"RouteZone":{"zone":1,"source":0}}"#).await;
    let audio = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::Audio(chunk) = recv(&mut stream).await {
                return chunk;
            }
        }
    })
    .await
    .expect("no audio within 5 s of routing");
    println!(
        "audio: stream {} seq {} {} bytes",
        audio.stream.0,
        audio.seq,
        audio.data.len()
    );

    drop(stream);
    client_state(&server, |c| c["online"] == false).await;
}
