use std::io::{ErrorKind, Read};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use bounce_common::ids::ClientId;
use bounce_common::protocol::{ClientMessage, PROTOCOL_VERSION};
use bounce_common::wire;

const ID: &str = "b0c00000000000000000000000000001";

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn says_hello_with_its_id_and_wav_output() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = listener.local_addr().unwrap().to_string();
    let _client = Running(
        Command::new(env!("CARGO_BIN_EXE_bounce-client"))
            .args(["--client-id", ID, "--server", &server])
            .args(["--data-dir", "/nonexistent", "--output", "wav:/nonexistent"])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("client never connected: {e}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).unwrap();
    let mut body = vec![0; u32::from_le_bytes(prefix) as usize];
    stream.read_exact(&mut body).unwrap();

    let ClientMessage::Hello(hello) = wire::decode(&body).unwrap() else {
        panic!("first message was not Hello");
    };
    assert_eq!(hello.protocol, PROTOCOL_VERSION);
    assert_eq!(hello.client, ClientId::parse(ID).unwrap());
    let version = hello.version;
    let version = format!("{}.{}.{}", version.major, version.minor, version.patch);
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert_ne!(version, "0.0.0", "the client crate has no version");
    assert_eq!(hello.outputs.len(), 1);
    assert_eq!(hello.outputs[0].id, "wav");
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    assert_eq!(hello.hostname, hostname.trim());
}
