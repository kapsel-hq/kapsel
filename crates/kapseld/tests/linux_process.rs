//! Linux real-process proof for the compile-time service test harness.

#![cfg(all(target_os = "linux", feature = "test-harness"))]
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "controlled process fixtures must fail the Linux gate immediately"
)]

use std::{
    fmt::Write as _,
    fs,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    ops::{Deref, DerefMut},
    os::unix::{
        fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
        net::{UnixListener as StdUnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use kapsel::{provision_exact_grant, ExactAuthorization, GrantProvisioning};
use sha2::Digest as _;

const IMAGE: &str = concat!(
    "registry.example/agent-api@sha256:",
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
);
const OLD_IMAGE: &str = concat!(
    "registry.example/agent-api@sha256:",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
);
const DEPLOYMENT_PATH: &str = "/apis/apps/v1/namespaces/demo/deployments/agent-api";
const PATCH_DEPLOYMENT_PATH: &str = "/apis/apps/v1/namespaces/demo/deployments/agent-api?";
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(10);

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn wait_with_output(mut self) -> std::io::Result<Output> {
        self.0.take().unwrap().wait_with_output()
    }
}

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kapseld-linux-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).unwrap();
    path
}

fn effective_uid() -> u32 {
    numeric_identity("-u")
}

fn effective_gid() -> u32 {
    numeric_identity("-g")
}

fn numeric_identity(argument: &str) -> u32 {
    let output = Command::new("id").arg(argument).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn kapseld_executable() -> PathBuf {
    std::env::var_os("KAPSELD_TEST_EXECUTABLE").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_kapseld")),
        PathBuf::from,
    )
}

fn spawn(socket: &Path, expected_gid: u32, connections: usize) -> ChildGuard {
    ChildGuard::new(
        Command::new(kapseld_executable())
            .env("KAPSELD_TEST_SOCKET", socket)
            .env("KAPSELD_TEST_EXPECTED_GID", expected_gid.to_string())
            .env("KAPSELD_TEST_CONNECTIONS", connections.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn spawn_installed(root: &Path, connections: usize) -> ChildGuard {
    spawn_installed_with_arguments(
        root,
        connections,
        &[
            "--operator-config",
            "/etc/kapsel/operator.json",
            "--socket",
            "/run/kapsel/kapseld.sock",
        ],
    )
}

fn spawn_installed_with_arguments(
    root: &Path,
    connections: usize,
    arguments: &[&str],
) -> ChildGuard {
    ChildGuard::new(
        installed_command(root, connections, arguments)
            .spawn()
            .unwrap(),
    )
}

fn spawn_installed_with_seam(root: &Path, connections: usize, seam: &str) -> ChildGuard {
    let mut command = installed_command(
        root,
        connections,
        &[
            "--operator-config",
            "/etc/kapsel/operator.json",
            "--socket",
            "/run/kapsel/kapseld.sock",
        ],
    );
    command
        .env("KAPSEL_DEMO_CONTROL_DIRECTORY", root.join("control"))
        .env("KAPSEL_DEMO_PAUSE", seam);
    ChildGuard::new(command.spawn().unwrap())
}

fn installed_command(root: &Path, connections: usize, arguments: &[&str]) -> Command {
    let mut command = Command::new(kapseld_executable());
    command
        .args(arguments)
        .env("KAPSELD_TEST_INSTALLATION_ROOT", root)
        .env("KAPSELD_TEST_CONNECTIONS", connections.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn spawn_application(
    socket: &Path,
    root: &Path,
    kubernetes_url: &str,
    receipt_configuration: &str,
    seam: Option<&str>,
    connections: usize,
) -> ChildGuard {
    let mut command = Command::new(kapseld_executable());
    command
        .env("KAPSELD_TEST_SOCKET", socket)
        .env("KAPSELD_TEST_EXPECTED_GID", effective_gid().to_string())
        .env("KAPSELD_TEST_CONNECTIONS", connections.to_string())
        .env("KAPSELD_TEST_APPLICATION_ROOT", root)
        .env("KAPSELD_TEST_KUBERNETES_URL", kubernetes_url)
        .env("KAPSELD_TEST_RECEIPT_CONFIGURATION", receipt_configuration)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(seam) = seam {
        command
            .env("KAPSEL_DEMO_CONTROL_DIRECTORY", root.join("control"))
            .env("KAPSEL_DEMO_PAUSE", seam);
    }
    ChildGuard::new(command.spawn().unwrap())
}

fn connect(socket: &Path) -> UnixStream {
    connect_with_timeout(socket, Duration::from_secs(5))
}

fn connect_with_timeout(socket: &Path, timeout: Duration) -> UnixStream {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => {
                stream.set_read_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
                return stream;
            },
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            },
            Err(error) => panic!("kapseld did not bind: {error}"),
        }
    }
}

fn wait_for_socket(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "kapseld did not bind");
        thread::sleep(Duration::from_millis(10));
    }
}

fn group_gid(group: &str) -> u32 {
    let output = Command::new("getent")
        .args(["group", group])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .split(':')
        .nth(2)
        .unwrap()
        .parse()
        .unwrap()
}

fn write_frame(stream: &mut UnixStream, body: &[u8]) {
    stream.set_write_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
    stream
        .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(body).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
}

fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    stream.set_read_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix).unwrap();
    let mut response = vec![0_u8; u32::from_be_bytes(prefix) as usize];
    stream.read_exact(&mut response).unwrap();
    response
}

fn assert_terminal_status(stream: &mut UnixStream, status: &str, snapshot: bool) {
    let target = serde_json::json!({"uid": "uid-1", "resource_version": "1"});
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&read_frame(stream)).unwrap(),
        serde_json::json!({
            "status": status,
            "approved_target": snapshot.then_some(&target),
            "attempt_target": target,
            "observed_target": {"uid": "uid-1", "resource_version": "3"}
        })
    );
}

fn lowercase_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").unwrap();
    }
    output
}

fn submit_request() -> String {
    format!(
        concat!(
            "{{\"request\":\"submit_set_deployment_image\",",
            "\"operation_id\":\"process-op\",\"namespace\":\"demo\",",
            "\"deployment\":\"agent-api\",\"container\":\"api\",",
            "\"immutable_image_digest\":\"{}\"}}"
        ),
        IMAGE
    )
}

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn installation_root(name: &str) -> PathBuf {
    installation_root_with_url(name, "http://127.0.0.1:1234")
}

fn installation_root_with_url(name: &str, kubernetes_url: &str) -> PathBuf {
    let root = root(name);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    for (directory, mode) in [
        ("etc", 0o700),
        ("etc/kapsel", 0o700),
        ("var", 0o700),
        ("var/lib", 0o700),
        ("var/lib/kapsel", 0o700),
        ("var/lib/kapsel/receipts", 0o700),
        ("run", 0o700),
        ("run/kapsel", 0o750),
        ("control", 0o700),
    ] {
        let path = root.join(directory);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    let root = fs::canonicalize(root).unwrap();
    let authorization_seed = [111_u8; 32];
    let authorization_key = SigningKey::from_bytes(&authorization_seed);
    let grant = provision_exact_grant(&GrantProvisioning {
        authorization: &ExactAuthorization {
            approved_target: Some(kapsel::ApprovedTarget {
                uid: "uid-1".into(),
                resource_version: "1".into(),
            }),
            authorization_id: "service-auth".into(),
            operation_id: "process-op".into(),
            namespace: "demo".into(),
            deployment: "agent-api".into(),
            container: "api".into(),
            immutable_image_digest: IMAGE.into(),
        },
        signing_seed: &authorization_seed,
        signing_key_id: "service-owner-key",
    })
    .unwrap();
    private_file(&root.join("etc/kapsel/grant.bin"), &grant);
    private_file(
        &root.join("etc/kapsel/authorization.pub"),
        &authorization_key.verifying_key().to_bytes(),
    );
    private_file(
        &root.join("etc/kapsel/kubeconfig.yaml"),
        format!(
            concat!(
                "apiVersion: v1\nkind: Config\ncurrent-context: fixture\n",
                "clusters:\n- name: fixture\n  cluster:\n",
                "    server: {}\n",
                "contexts:\n- name: fixture\n  context:\n",
                "    cluster: fixture\n    user: fixture\n",
                "users:\n- name: fixture\n  user: {{}}\n"
            ),
            kubernetes_url
        )
        .as_bytes(),
    );
    private_file(&root.join("etc/kapsel/receipt.seed"), &[112_u8; 32]);
    private_file(
        &root.join("etc/kapsel/operator.json"),
        &serde_json::to_vec(&serde_json::json!({
            "signed_authorization_grant": root.join("etc/kapsel/grant.bin"),
            "authorization_key_id": "service-owner-key",
            "authorization_public_key": root.join("etc/kapsel/authorization.pub"),
            "kubeconfig": root.join("etc/kapsel/kubeconfig.yaml"),
            "journal": root.join("var/lib/kapsel/journal.sqlite3"),
            "receipt_directory": root.join("var/lib/kapsel/receipts"),
            "receipt_signing_seed": root.join("etc/kapsel/receipt.seed"),
            "receipt_signing_key_id": "service-receipt-key"
        }))
        .unwrap(),
    );
    root
}

fn application_root(name: &str) -> PathBuf {
    let root = root(name);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    for directory in ["control", "receipts-a", "receipts-b"] {
        let path = root.join(directory);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::canonicalize(root).unwrap()
}

fn deployment(resource_version: &str, generation: i64, observed: bool) -> String {
    let mut value = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": "agent-api",
            "namespace": "demo",
            "uid": "uid-1",
            "resourceVersion": resource_version,
            "generation": generation
        },
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "agent-api"}},
            "template": {
                "metadata": {"labels": {"app": "agent-api"}},
                "spec": {"containers": [{
                    "name": "api",
                    "image": if generation == 1 { OLD_IMAGE } else { IMAGE }
                }]}
            }
        }
    });
    if observed {
        value["metadata"]["annotations"] = serde_json::json!({
            "kapsel.dev/kap0038-operation-id": "process-op"
        });
        value["status"] = serde_json::json!({
            "observedGeneration": 2,
            "updatedReplicas": 1,
            "availableReplicas": 1,
            "unavailableReplicas": 0,
            "conditions": [{
                "type": "Available",
                "status": "True",
                "reason": "MinimumReplicasAvailable"
            }]
        });
    }
    value.to_string()
}

fn progressing_deployment() -> String {
    let mut value: serde_json::Value = serde_json::from_str(&deployment("3", 2, false)).unwrap();
    value["metadata"]["annotations"] = serde_json::json!({
        "kapsel.dev/kap0038-operation-id": "process-op"
    });
    value["status"] = serde_json::json!({
        "observedGeneration": 2,
        "updatedReplicas": 0,
        "availableReplicas": 0,
        "unavailableReplicas": 1,
        "conditions": [{
            "type": "Progressing",
            "status": "True",
            "reason": "ReplicaSetUpdated"
        }]
    });
    value.to_string()
}

fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "provider fixture accept timed out"
                );
                thread::sleep(Duration::from_millis(10));
            },
            Err(error) => panic!("provider fixture accept failed: {error}"),
        }
    }
}

fn read_http_request(listener: &TcpListener) -> (TcpStream, String, String, Vec<u8>) {
    read_http_stream(accept_with_timeout(listener))
}

fn read_http_stream(mut stream: TcpStream) -> (TcpStream, String, String, Vec<u8>) {
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
    let mut bytes = Vec::new();
    let total = loop {
        let mut chunk = [0_u8; 4096];
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("provider fixture read failed: {error}"),
        };
        assert!(
            read > 0,
            "provider fixture request ended before framing completed"
        );
        bytes.extend_from_slice(&chunk[..read]);
        assert!(
            bytes.len() <= 16 * 1024,
            "provider fixture request exceeded bound"
        );
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header_end = header_end + 4;
        let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
        let total = header_end + content_length;
        if bytes.len() >= total {
            break total;
        }
    };
    assert_eq!(
        bytes.len(),
        total,
        "provider fixture rejected trailing request bytes"
    );
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let request_line = std::str::from_utf8(&bytes[..header_end])
        .unwrap()
        .lines()
        .next()
        .unwrap();
    let mut fields = request_line.split_ascii_whitespace();
    let method = fields.next().unwrap().to_owned();
    let path = fields.next().unwrap().to_owned();
    assert_eq!(fields.next(), Some("HTTP/1.1"));
    assert_eq!(fields.next(), None);
    (stream, method, path, bytes[header_end..].to_vec())
}

fn assert_provider_request(
    method: &str,
    path: &str,
    body: &[u8],
    expected_method: &str,
    expected_path: &str,
) {
    assert_eq!(method, expected_method);
    assert_eq!(path, expected_path);
    if expected_method == "PATCH" {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(body).unwrap(),
            serde_json::json!({
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {
                    "name": "agent-api",
                    "namespace": "demo",
                    "uid": "uid-1",
                    "resourceVersion": "1",
                    "annotations": {
                        "kapsel.dev/kap0038-operation-id": "process-op"
                    }
                },
                "spec": {
                    "template": {
                        "spec": {
                            "containers": [{"name": "api", "image": IMAGE}]
                        }
                    }
                }
            })
        );
    } else {
        assert!(body.is_empty());
    }
}

fn write_http_response(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n",
            "content-length: {}\r\nconnection: close\r\n\r\n"
        ),
        body.len()
    )
    .unwrap();
    stream.write_all(body.as_bytes()).unwrap();
}

struct SuccessServer {
    url: String,
    patch_count: Arc<AtomicUsize>,
    request_count: Arc<AtomicUsize>,
    stop: mpsc::Sender<()>,
    observation_started: mpsc::Receiver<()>,
    release_observation: mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

impl SuccessServer {
    fn finish(self) {
        let _ = self.stop.send(());
        self.thread.join().unwrap();
        assert_eq!(self.request_count.load(Ordering::Relaxed), 3);
        assert_eq!(self.patch_count.load(Ordering::Relaxed), 1);
    }
}

fn success_server() -> SuccessServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let patch_count = Arc::new(AtomicUsize::new(0));
    let server_patch_count = patch_count.clone();
    let request_count = Arc::new(AtomicUsize::new(0));
    let server_request_count = request_count.clone();
    let (stop, stopped) = mpsc::channel();
    let (observation_started_tx, observation_started) = mpsc::channel();
    let (release_observation, release_observation_rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        for (index, (expected_method, expected_path, response_body)) in [
            ("GET", DEPLOYMENT_PATH, deployment("1", 1, false)),
            ("PATCH", PATCH_DEPLOYMENT_PATH, deployment("2", 2, false)),
            ("GET", DEPLOYMENT_PATH, deployment("3", 2, true)),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut stream, method, path, request_body) = read_http_request(&listener);
            server_request_count.fetch_add(1, Ordering::Relaxed);
            if method == "PATCH" {
                server_patch_count.fetch_add(1, Ordering::Relaxed);
            }
            assert_provider_request(
                &method,
                &path,
                &request_body,
                expected_method,
                expected_path,
            );
            if index == 2 {
                observation_started_tx.send(()).unwrap();
                release_observation_rx
                    .recv_timeout(FIXTURE_TIMEOUT)
                    .unwrap();
            }
            write_http_response(&mut stream, &response_body);
        }
        // Remain a live receiver until every restarted process and client has exited.
        // Drain queued connections before accepting shutdown, so late calls cannot hide.
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (_, method, path, _) = read_http_stream(stream);
                    server_request_count.fetch_add(1, Ordering::Relaxed);
                    if method == "PATCH" {
                        server_patch_count.fetch_add(1, Ordering::Relaxed);
                    }
                    panic!("unexpected provider request after completion: {method} {path}");
                },
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    match stopped.try_recv() {
                        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {},
                    }
                    thread::sleep(Duration::from_millis(10));
                },
                Err(error) => panic!("provider fixture late-request check failed: {error}"),
            }
        }
    });
    SuccessServer {
        url,
        patch_count,
        request_count,
        stop,
        observation_started,
        release_observation,
        thread,
    }
}

fn unknown_server() -> (String, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let patch_count = Arc::new(AtomicUsize::new(0));
    let server_patch_count = patch_count.clone();
    let thread = thread::spawn(move || {
        for index in 0..32 {
            let expected_method = if index == 1 { "PATCH" } else { "GET" };
            let expected_path = if index == 1 {
                PATCH_DEPLOYMENT_PATH
            } else {
                DEPLOYMENT_PATH
            };
            let response_body = match index {
                0 => deployment("1", 1, false),
                1 => deployment("2", 2, false),
                _ => progressing_deployment(),
            };
            let (mut stream, method, path, request_body) = read_http_request(&listener);
            assert_provider_request(
                &method,
                &path,
                &request_body,
                expected_method,
                expected_path,
            );
            if expected_method == "PATCH" {
                server_patch_count.fetch_add(1, Ordering::Relaxed);
            }
            write_http_response(&mut stream, &response_body);
        }
        let deadline = Instant::now() + Duration::from_millis(1_500);
        loop {
            match listener.accept() {
                Ok(_) => panic!("provider fixture received more than 30 recovery reads"),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                },
                Err(error) => panic!("provider fixture extra-read check failed: {error}"),
            }
        }
    });
    (url, patch_count, thread)
}

fn kill(child: &mut ChildGuard) {
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    child.0.take();
}

fn wait_for_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "kapseld did not reach fault seam"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "kapseld exited before fault seam"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ordinary_restart_reconciles_before_replacing_stale_socket_without_second_patch() {
    let server = success_server();
    let root = installation_root_with_url("ordinary-recovery", &server.url);
    let socket = root.join("run/kapsel/kapseld.sock");
    let mut first = spawn_installed_with_seam(&root, 10, "after_apply");
    let mut submit = connect(&socket);
    write_frame(&mut submit, submit_request().as_bytes());
    assert_eq!(read_frame(&mut submit), br#"{"status":"ACCEPTED"}"#);
    wait_for_marker(&mut first, &root.join("control/after-apply.ready"));
    kill(&mut first);
    assert!(socket.exists());

    let child = spawn_installed(&root, 1);
    server
        .observation_started
        .recv_timeout(FIXTURE_TIMEOUT)
        .unwrap();
    let error = UnixStream::connect(&socket).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
    server.release_observation.send(()).unwrap();

    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_terminal_status(&mut status, "SUCCEEDED", true);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    server.finish();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ordinary_startup_rejects_every_nonexact_or_active_socket_without_unlinking() {
    for (name, kind) in [
        ("regular", "regular"),
        ("directory", "directory"),
        ("symlink", "symlink"),
        ("wrong-mode", "wrong-mode"),
        ("linked", "linked"),
        ("active", "active"),
    ] {
        let root = installation_root(name);
        let socket = root.join("run/kapsel/kapseld.sock");
        let mut active = None;
        match kind {
            "regular" => private_file(&socket, b"not a socket"),
            "directory" => fs::create_dir(&socket).unwrap(),
            "symlink" => {
                let target = root.join("run/kapsel/target");
                private_file(&target, b"target");
                std::os::unix::fs::symlink(target, &socket).unwrap();
            },
            "wrong-mode" => {
                let listener = StdUnixListener::bind(&socket).unwrap();
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
                drop(listener);
            },
            "linked" => {
                let listener = StdUnixListener::bind(&socket).unwrap();
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o660)).unwrap();
                drop(listener);
                fs::hard_link(&socket, root.join("run/kapsel/linked.sock")).unwrap();
            },
            "active" => {
                let listener = StdUnixListener::bind(&socket).unwrap();
                fs::set_permissions(&socket, fs::Permissions::from_mode(0o660)).unwrap();
                active = Some(listener);
            },
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&socket).unwrap();
        let output = spawn_installed(&root, 1).wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(4));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let after = fs::symlink_metadata(&socket).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        drop(active);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ordinary_startup_accepts_only_the_exact_ordered_arguments() {
    let variants: [&[&str]; 5] = [
        &[],
        &["--operator-config", "/etc/kapsel/operator.json"],
        &[
            "--socket",
            "/run/kapsel/kapseld.sock",
            "--operator-config",
            "/etc/kapsel/operator.json",
        ],
        &[
            "--operator-config",
            "/tmp/operator.json",
            "--socket",
            "/run/kapsel/kapseld.sock",
        ],
        &[
            "--operator-config",
            "/etc/kapsel/operator.json",
            "--socket",
            "/run/kapsel/kapseld.sock",
            "extra",
        ],
    ];
    for (index, arguments) in variants.into_iter().enumerate() {
        let root = installation_root(&format!("arguments-{index}"));
        let output = spawn_installed_with_arguments(&root, 1, arguments)
            .wait_with_output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!root.join("run/kapsel/kapseld.sock").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ordinary_startup_removes_only_an_exact_inactive_stale_socket() {
    let root = installation_root("stale-socket");
    let socket = root.join("run/kapsel/kapseld.sock");
    let stale = StdUnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o660)).unwrap();
    drop(stale);

    let child = spawn_installed(&root, 1);
    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_eq!(read_frame(&mut status), br#"{"status":"NOT_FOUND"}"#);
    assert!(child.wait_with_output().unwrap().status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ordinary_startup_uses_fixed_inputs_and_serves_until_systemd_termination() {
    let root = installation_root("ordinary-startup");
    let socket = root.join("run/kapsel/kapseld.sock");
    let child = spawn_installed(&root, 1);
    wait_for_socket(&socket);
    let metadata = fs::symlink_metadata(&socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.uid(), effective_uid());
    assert_eq!(metadata.gid(), effective_gid());
    assert_eq!(metadata.mode() & 0o7777, 0o660);
    assert_eq!(metadata.nlink(), 1);
    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_eq!(read_frame(&mut status), br#"{"status":"NOT_FOUND"}"#);

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(socket.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restart_reconciles_before_bind_without_a_second_patch() {
    let root = application_root("startup-reconcile");
    let socket = root.join("kapseld.sock");
    let server = success_server();

    let mut first = spawn_application(&socket, &root, &server.url, "A", Some("after_apply"), 10);
    let mut submit = connect(&socket);
    write_frame(&mut submit, submit_request().as_bytes());
    assert_eq!(read_frame(&mut submit), br#"{"status":"ACCEPTED"}"#);
    wait_for_marker(&mut first, &root.join("control/after-apply.ready"));
    kill(&mut first);
    fs::remove_file(&socket).unwrap();

    let child = spawn_application(&socket, &root, &server.url, "A", Some("after_apply"), 1);
    server
        .observation_started
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert!(!socket.exists());
    server.release_observation.send(()).unwrap();

    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_terminal_status(&mut status, "SUCCEEDED", false);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!socket.exists());
    server.finish();
    assert_eq!(
        fs::read_to_string(root.join("control/provider-apply-count")).unwrap(),
        "1"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one process trace proves snapshot recovery and export without replay"
)]
fn ordinary_restart_reuses_frozen_snapshot_receipt_under_rotated_configuration() {
    let server = success_server();
    let root = installation_root_with_url("receipt-recovery", &server.url);
    let socket = root.join("run/kapsel/kapseld.sock");
    let journal = root.join("var/lib/kapsel/journal.sqlite3");
    let receipts = root.join("var/lib/kapsel/receipts");
    fs::remove_dir(&receipts).unwrap();
    let document_path = root.join("etc/kapsel/operator.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&document_path).unwrap()).unwrap();
    document
        .as_object_mut()
        .unwrap()
        .remove("receipt_directory");
    private_file(&document_path, &serde_json::to_vec(&document).unwrap());

    let mut first = spawn_installed_with_seam(&root, 10, "after_apply");
    let mut submit = connect(&socket);
    write_frame(&mut submit, submit_request().as_bytes());
    assert_eq!(read_frame(&mut submit), br#"{"status":"ACCEPTED"}"#);
    wait_for_marker(&mut first, &root.join("control/after-apply.ready"));
    kill(&mut first);
    assert!(socket.exists());

    let mut publication = spawn_installed_with_seam(&root, 10, "after_receipt_commit");
    server
        .observation_started
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        UnixStream::connect(&socket).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    server.release_observation.send(()).unwrap();
    wait_for_marker(
        &mut publication,
        &root.join("control/after-receipt-commit.ready"),
    );
    assert_eq!(
        UnixStream::connect(&socket).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    let connection = rusqlite::Connection::open(&journal).unwrap();
    let (state, frozen, signer): (String, Vec<u8>, String) = connection
        .query_row(
            "SELECT state, receipt_bytes, receipt_key_id FROM kubernetes_image_operations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "finalized");
    assert_eq!(signer, "service-receipt-key");
    drop(connection);
    kill(&mut publication);

    private_file(&root.join("etc/kapsel/receipt.seed"), &[113_u8; 32]);
    document["receipt_signing_key_id"] = "rotated-receipt-key".into();
    document["receipt_directory"] = serde_json::json!(root.join("missing-rotated-receipts"));
    private_file(&document_path, &serde_json::to_vec(&document).unwrap());
    let child = spawn_installed(&root, 9);
    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_terminal_status(&mut status, "SUCCEEDED", true);
    let mut receipt = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut receipt,
        br#"{"request":"get_set_deployment_image_receipt","operation_id":"process-op"}"#,
    );
    let response: serde_json::Value = serde_json::from_slice(&read_frame(&mut receipt)).unwrap();
    let expected_hex = lowercase_hex(&frozen);
    let expected_digest = lowercase_hex(&sha2::Sha256::digest(&frozen));
    assert_eq!(response["status"], "READY");
    assert_eq!(response["receipt_hex"], expected_hex);
    assert_eq!(response["receipt_sha256"], expected_digest);
    let run_client = |destination: &Path| {
        Command::new(env!("CARGO_BIN_EXE_kapsel-service-client"))
            .env("KAPSELD_TEST_CLIENT_SOCKET", &socket)
            .args(["receipt", "process-op", destination.to_str().unwrap()])
            .output()
            .unwrap()
    };
    assert_eq!(
        run_client(&root.join("unavailable/receipt")).status.code(),
        Some(4)
    );
    let exported = root.join("exported.receipt");
    assert!(run_client(&exported).status.success());
    assert_eq!(run_client(&exported).status.code(), Some(4));
    let repeated = root.join("repeated.receipt");
    assert!(run_client(&repeated).status.success());
    assert_eq!(fs::read(&exported).unwrap(), frozen);
    assert_eq!(fs::read(&repeated).unwrap(), frozen);
    let trust = kapsel::ReceiptTrust {
        key_id: signer.clone(),
        public_key: SigningKey::from_bytes(&[112_u8; 32])
            .verifying_key()
            .to_bytes(),
        accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v3".into(),
        not_before_unix_s: 100,
        not_after_unix_s: 200,
    }
    .encode()
    .unwrap();
    assert_eq!(
        kapsel::inspect_receipt(&frozen, &trust, 150, kapsel::InspectionLimits::default()).status(),
        kapsel::InspectionStatus::Inspected
    );
    let mut replay = connect(&socket);
    write_frame(&mut replay, submit_request().as_bytes());
    assert_eq!(read_frame(&mut replay), br#"{"status":"ACCEPTED"}"#);
    let mut status = connect(&socket);
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_terminal_status(&mut status, "SUCCEEDED", true);
    let mut receipt = connect(&socket);
    write_frame(
        &mut receipt,
        br#"{"request":"get_set_deployment_image_receipt","operation_id":"process-op"}"#,
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&read_frame(&mut receipt)).unwrap(),
        response
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!receipts.exists());
    assert!(!root.join("missing-rotated-receipts").exists());
    let connection = rusqlite::Connection::open(&journal).unwrap();
    let retained: (Vec<u8>, String) = connection
        .query_row(
            "SELECT receipt_bytes, receipt_key_id FROM kubernetes_image_operations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(retained, (frozen, signer));
    server.finish();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_reconciliation_failure_is_silent_and_leaves_no_socket() {
    let root = application_root("reconciliation-failure");
    let socket = root.join("kapseld.sock");
    let server = success_server();

    let mut first = spawn_application(&socket, &root, &server.url, "A", Some("after_apply"), 10);
    let mut submit = connect(&socket);
    write_frame(&mut submit, submit_request().as_bytes());
    assert_eq!(read_frame(&mut submit), br#"{"status":"ACCEPTED"}"#);
    wait_for_marker(&mut first, &root.join("control/after-apply.ready"));
    kill(&mut first);
    fs::remove_file(&socket).unwrap();

    let mut publication = spawn_application(
        &socket,
        &root,
        &server.url,
        "A",
        Some("after_receipt_commit"),
        10,
    );
    server
        .observation_started
        .recv_timeout(FIXTURE_TIMEOUT)
        .unwrap();
    assert!(!socket.exists());
    server.release_observation.send(()).unwrap();
    wait_for_marker(
        &mut publication,
        &root.join("control/after-receipt-commit.ready"),
    );
    kill(&mut publication);
    let connection = rusqlite::Connection::open(root.join("journal.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE kubernetes_image_operations SET receipt_bytes = ?1",
            [b"different frozen receipt bytes".as_slice()],
        )
        .unwrap();
    drop(connection);

    let child = spawn_application(&socket, &root, &server.url, "B", None, 1);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!socket.exists());
    server.finish();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restart_preserves_unknown_after_bounded_observation_without_a_second_patch() {
    let root = application_root("unknown-recovery");
    let socket = root.join("kapseld.sock");
    let (url, patch_count, server) = unknown_server();

    let mut first = spawn_application(&socket, &root, &url, "A", Some("after_apply"), 10);
    let mut submit = connect(&socket);
    write_frame(&mut submit, submit_request().as_bytes());
    assert_eq!(read_frame(&mut submit), br#"{"status":"ACCEPTED"}"#);
    wait_for_marker(&mut first, &root.join("control/after-apply.ready"));
    kill(&mut first);
    fs::remove_file(&socket).unwrap();

    let child = spawn_application(&socket, &root, &url, "A", None, 2);
    let mut status = connect_with_timeout(&socket, Duration::from_secs(40));
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_terminal_status(&mut status, "UNKNOWN", false);
    let mut receipt = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut receipt,
        br#"{"request":"get_set_deployment_image_receipt","operation_id":"process-op"}"#,
    );
    let response: serde_json::Value = serde_json::from_slice(&read_frame(&mut receipt)).unwrap();
    assert_eq!(response["status"], "READY");
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    server.join().unwrap();
    assert_eq!(patch_count.load(Ordering::Relaxed), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn startup_failure_is_silent_and_leaves_no_socket() {
    let root = application_root("startup-failure");
    let socket = root.join("kapseld.sock");
    let child = spawn_application(&socket, &root, "http://example.com:1234", "A", None, 1);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!socket.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn matching_effective_gid_crosses_the_real_kapseld_process() {
    let root = root("allow");
    let socket = root.join("kapseld.sock");
    let child = spawn(&socket, effective_gid(), 1);
    let mut stream = connect(&socket);
    write_frame(
        &mut stream,
        br#"{"request":"get_set_deployment_image_status","operation_id":"missing"}"#,
    );
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert_eq!(
        response,
        [
            &(u32::try_from(br#"{"status":"NOT_FOUND"}"#.len())
                .unwrap()
                .to_be_bytes())[..],
            br#"{"status":"NOT_FOUND"}"#,
        ]
        .concat()
    );
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!socket.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disconnect_busy_and_reconnect_status_cross_the_real_process() {
    let root = root("execution");
    let socket = root.join("kapseld.sock");
    let child = spawn(&socket, effective_gid(), 4);

    let mut disconnected = connect(&socket);
    write_frame(&mut disconnected, submit_request().as_bytes());
    drop(disconnected);

    let mut status = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut status,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_eq!(read_frame(&mut status), br#"{"status":"IN_PROGRESS"}"#);

    let mut competing = UnixStream::connect(&socket).unwrap();
    write_frame(&mut competing, submit_request().as_bytes());
    assert_eq!(read_frame(&mut competing), br#"{"status":"BUSY"}"#);

    let mut completed = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut completed,
        br#"{"request":"get_set_deployment_image_status","operation_id":"process-op"}"#,
    );
    assert_eq!(
        read_frame(&mut completed),
        br#"{"status":"NOT_ATTEMPTED","target_rejection":"DEPLOYMENT_NOT_FOUND"}"#
    );

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn saturated_ninth_is_closed_and_new_tenth_succeeds_after_recovery() {
    let root = root("saturation");
    let socket = root.join("kapseld.sock");
    let child = spawn(&socket, effective_gid(), 10);
    let mut admitted = vec![connect(&socket)];
    for _ in 1..8 {
        admitted.push(UnixStream::connect(&socket).unwrap());
    }
    thread::sleep(Duration::from_millis(50));

    let mut ninth = UnixStream::connect(&socket).unwrap();
    ninth
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut denied = Vec::new();
    match ninth.read_to_end(&mut denied) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {},
        Err(error) => panic!("unexpected saturated-peer read failure: {error}"),
    }
    assert!(denied.is_empty());

    drop(admitted.remove(0));
    thread::sleep(Duration::from_millis(50));
    let mut tenth = UnixStream::connect(&socket).unwrap();
    write_frame(
        &mut tenth,
        br#"{"request":"get_set_deployment_image_status","operation_id":"tenth"}"#,
    );
    tenth
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut prefix = [0_u8; 4];
    tenth.read_exact(&mut prefix).unwrap();
    let mut response = vec![0_u8; u32::from_be_bytes(prefix) as usize];
    tenth.read_exact(&mut response).unwrap();
    assert_eq!(response, br#"{"status":"NOT_FOUND"}"#);
    drop(admitted);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires an existing supplementary docker group and sg on the authorized Linux host"]
fn distinct_effective_gid_is_denied_before_frame_read() {
    let root = root("distinct-gid");
    let socket = root.join("kapseld.sock");
    let server_gid = effective_gid();
    let client_gid = group_gid("docker");
    assert_ne!(server_gid, client_gid);
    let child = spawn(&socket, server_gid, 1);
    wait_for_socket(&socket);

    let client = root.join("client.py");
    fs::write(
        &client,
        concat!(
            "import os, socket, time\n",
            "assert os.getegid() == int(os.environ['KAPSELD_DISTINCT_GID'])\n",
            "stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n",
            "stream.settimeout(1.0)\n",
            "started = time.monotonic()\n",
            "stream.connect(os.environ['KAPSELD_DISTINCT_GID_SOCKET'])\n",
            "assert stream.recv(1) == b''\n",
            "assert time.monotonic() - started < 1.0\n",
        ),
    )
    .unwrap();
    let command = format!("python3 {}", client.display());
    let output = Command::new("sg")
        .args(["docker", "-c", &command])
        .env("KAPSELD_DISTINCT_GID", client_gid.to_string())
        .env("KAPSELD_DISTINCT_GID_SOCKET", &socket)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let server_output = child.wait_with_output().unwrap();
    assert!(server_output.status.success());
    assert!(server_output.stdout.is_empty());
    assert!(server_output.stderr.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn different_expected_gid_is_denied_before_body_disclosure() {
    let root = root("deny");
    let socket = root.join("kapseld.sock");
    let expected_gid = effective_gid().wrapping_add(1);
    let child = spawn(&socket, expected_gid, 1);
    let mut stream = connect(&socket);
    let _ = stream.write_all(b"SECRET_UNAUTHENTICATED_BODY");
    let mut response = Vec::new();
    match stream.read_to_end(&mut response) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {},
        Err(error) => panic!("unexpected denied-peer read failure: {error}"),
    }
    assert!(response.is_empty());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    fs::remove_dir_all(root).unwrap();
}
