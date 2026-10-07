use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};

use work_tracker::{
    db::{NewWorkItem, Tracker},
    domain::{Schedule, Status},
};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request(address: &str, method: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn dashboard_routes_display_escaped_directories_and_reject_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("web.db");
    let mut tracker = Tracker::open(&database).unwrap();
    let item = tracker
        .create_item(
            NewWorkItem {
                title: "Visible",
                description: None,
                status: Status::Pending,
                schedule: Schedule::default(),
                workdir: Some("/work/<project&'\">"),
            },
            "test",
            None,
        )
        .unwrap();
    let unknown = tracker
        .create("Unknown", None, Status::Pending, "test", None)
        .unwrap();
    drop(tracker);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .arg("--database")
            .arg(&database)
            .args(["serve", "--bind", &address])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut ready = false;
    for _ in 0..200 {
        if TcpStream::connect(&address).is_ok() {
            ready = true;
            break;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "dashboard exited before startup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready, "dashboard did not start");
    for path in ["/".to_owned(), format!("/items/{}", item.id)] {
        let response = request(&address, "GET", &path);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("Work directory:"));
        assert!(response.contains("/work/&lt;project&amp;&#39;&quot;&gt;"));
        assert!(!response.contains("/work/<project"));
    }
    assert!(request(&address, "GET", &format!("/items/{}", unknown.id)).contains("unknown"));
    assert!(request(&address, "POST", "/").starts_with("HTTP/1.1 405"));
    assert_eq!(
        Tracker::open(&database)
            .unwrap()
            .history(item.id)
            .unwrap()
            .len(),
        1
    );
}
