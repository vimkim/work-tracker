use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};

use work_tracker::web::host_allowed;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(extra: &[&str]) -> (tempfile::TempDir, Server, String) {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("web.db");
    let output = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(&database)
        .args(["add", "Secret ledger entry", "--actor", "web"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .arg("--database")
            .arg(&database)
            .args(["serve", "--bind", &address])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    for _ in 0..200 {
        if TcpStream::connect(&address).is_ok() {
            return (temp, server, address);
        }
        assert!(server.0.try_wait().unwrap().is_none(), "dashboard exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("dashboard did not start");
}

fn get(address: &str, host: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn rebinding_hostnames_cannot_read_the_dashboard() {
    let (_temp, _server, address) = start(&[]);
    let response = get(&address, "evil.example:8787");
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(!response.contains("Secret ledger entry"));
    assert!(response.contains("x-frame-options: DENY"), "{response}");
}

#[test]
fn loopback_names_on_any_tunnel_port_are_served_with_framing_blocked() {
    let (_temp, _server, address) = start(&[]);
    for host in [
        address.as_str(),
        "localhost:9000",
        "[::1]:8787",
        "LOCALHOST",
    ] {
        let response = get(&address, host);
        assert!(response.starts_with("HTTP/1.1 200"), "{host}: {response}");
        assert!(response.contains("Secret ledger entry"));
        assert!(response.contains("content-security-policy: frame-ancestors 'none'"));
        assert!(response.contains("x-content-type-options: nosniff"));
    }
}

#[test]
fn allow_host_admits_an_explicit_name() {
    let (_temp, _server, address) = start(&["--allow-host", "devbox.lan"]);
    assert!(get(&address, "devbox.lan:8787").starts_with("HTTP/1.1 200"));
    assert!(get(&address, "other.lan:8787").starts_with("HTTP/1.1 403"));
}

#[test]
fn host_names_are_parsed_without_trusting_lookalikes() {
    let none: &[String] = &[];
    for host in [
        "localhost",
        "localhost.",
        "127.0.0.1:1",
        "127.8.9.10",
        "[::1]",
    ] {
        assert!(host_allowed(host, none), "{host}");
    }
    for host in [
        "localhost.evil.example",
        "evil-localhost:80",
        "127.0.0.1.evil.example",
        "[::2]:80",
        "0.0.0.0",
        "",
    ] {
        assert!(!host_allowed(host, none), "{host}");
    }
}
