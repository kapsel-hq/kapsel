//! Black-box real-process proof for the two fixed demo-harness crash seams.

#![cfg(feature = "demo-harness")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "controlled fixture failures must fail the end-to-end test immediately"
)]

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use kapsel::{provision_exact_grant, ExactAuthorization, GrantProvisioning, ReceiptTrust};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
const IMAGE: &str = concat!(
    "registry.example/agent-api@sha256:",
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
);
const OLD_IMAGE: &str = concat!(
    "registry.example/agent-api@sha256:",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
);

struct Fixture {
    root: PathBuf,
    request: PathBuf,
    operator_a: PathBuf,
    operator_b: PathBuf,
    control: PathBuf,
    receipts_a: PathBuf,
    receipts_b: PathBuf,
    trust: PathBuf,
    server: Option<thread::JoinHandle<(usize, usize)>>,
    stop_server: mpsc::Sender<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
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
            "kapsel.dev/kap0038-operation-id": "demo-op-1"
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

#[allow(
    clippy::too_many_lines,
    reason = "the black-box fixture keeps one exact operator composition visible"
)]
fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "kapsel-e2e-demo-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    private_directory(&root);
    let root = fs::canonicalize(root).unwrap();
    let control = root.join("control");
    let receipts_a = root.join("receipts-a");
    let receipts_b = root.join("receipts-b");
    for directory in [&control, &receipts_a, &receipts_b] {
        private_directory(directory);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop_server, stopped) = mpsc::channel();
    let server = thread::spawn(move || {
        let responses = [
            ("GET", deployment("1", 1, false)),
            ("PATCH", deployment("2", 2, false)),
            ("GET", deployment("3", 2, true)),
        ];
        let mut requests = 0;
        let mut patches = 0;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    match stopped.try_recv() {
                        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) => {},
                    }
                    assert!(Instant::now() < deadline, "provider fixture timed out");
                    thread::sleep(Duration::from_millis(10));
                    continue;
                },
                Err(error) => panic!("provider accept failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_http_request(&mut stream);
            let method = request.split_ascii_whitespace().next().unwrap();
            requests += 1;
            if method == "PATCH" {
                patches += 1;
            }
            assert!(
                requests <= responses.len(),
                "unexpected late request: {request}"
            );
            let (expected_method, body) = &responses[requests - 1];
            assert_eq!(method, *expected_method);
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
        (requests, patches)
    });

    let authorization_seed = [9_u8; 32];
    let authorization_key = SigningKey::from_bytes(&authorization_seed);
    let authorization = ExactAuthorization {
        approved_target: None,
        authorization_id: "demo-auth-1".into(),
        operation_id: "demo-op-1".into(),
        namespace: "demo".into(),
        deployment: "agent-api".into(),
        container: "api".into(),
        immutable_image_digest: IMAGE.into(),
    };
    let grant = provision_exact_grant(&GrantProvisioning {
        authorization: &authorization,
        signing_seed: &authorization_seed,
        signing_key_id: "demo-authorization-key",
    })
    .unwrap();
    private_file(&root.join("grant.bin"), &grant);
    private_file(
        &root.join("authorization.pub"),
        &authorization_key.verifying_key().to_bytes(),
    );
    private_file(&root.join("receipt-a.seed"), &[9_u8; 32]);
    private_file(&root.join("receipt-b.seed"), &[8_u8; 32]);
    private_file(
        &root.join("kubeconfig.yaml"),
        format!(
            concat!(
                "apiVersion: v1\nkind: Config\nclusters:\n- name: fixture\n",
                "  cluster:\n    server: http://{address}\ncontexts:\n- name: fixture\n",
                "  context:\n    cluster: fixture\n    user: fixture\n",
                "current-context: fixture\nusers:\n- name: fixture\n  user: {{}}\n"
            ),
            address = address
        )
        .as_bytes(),
    );
    let request = root.join("request.json");
    private_file(
        &request,
        format!(
            concat!(
                "{{\"operation_id\":\"demo-op-1\",\"namespace\":\"demo\",",
                "\"deployment\":\"agent-api\",\"container\":\"api\",",
                "\"immutable_image_digest\":\"{IMAGE}\"}}"
            ),
            IMAGE = IMAGE
        )
        .as_bytes(),
    );
    let operator_a = root.join("operator-a.json");
    let operator_b = root.join("operator-b.json");
    write_operator(
        &root,
        &operator_a,
        &receipts_a,
        "receipt-a.seed",
        "effect-gateway-test-key",
    );
    write_operator(
        &root,
        &operator_b,
        &receipts_b,
        "receipt-b.seed",
        "rotated-receipt-key",
    );
    let trust = root.join("receipt.trust");
    private_file(
        &trust,
        &ReceiptTrust {
            key_id: "effect-gateway-test-key".into(),
            public_key: SigningKey::from_bytes(&[9_u8; 32])
                .verifying_key()
                .to_bytes(),
            accepted_purpose: "kapsel.kap0038.kubernetes-effect-receipt.v2".into(),
            not_before_unix_s: 100,
            not_after_unix_s: 200,
        }
        .encode()
        .unwrap(),
    );
    Fixture {
        root,
        request,
        operator_a,
        operator_b,
        control,
        receipts_a,
        receipts_b,
        trust,
        server: Some(server),
        stop_server,
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("provider read failed: {error}"),
        };
        assert!(read > 0, "incomplete provider request");
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() <= 16 * 1024, "provider request exceeded bound");
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
        if bytes.len() >= header_end + content_length {
            assert_eq!(bytes.len(), header_end + content_length);
            return headers.lines().next().unwrap().to_owned();
        }
    }
}

fn write_operator(
    root: &Path,
    path: &Path,
    receipts: &Path,
    seed_name: &str,
    receipt_key_id: &str,
) {
    private_file(
        path,
        format!(
            concat!(
                "{{\"signed_authorization_grant\":\"{}/grant.bin\",",
                "\"authorization_key_id\":\"demo-authorization-key\",",
                "\"authorization_public_key\":\"{}/authorization.pub\",",
                "\"kubeconfig\":\"{}/kubeconfig.yaml\",",
                "\"journal\":\"{}/journal.sqlite3\",",
                "\"receipt_directory\":\"{}\",",
                "\"receipt_signing_seed\":\"{}/{seed_name}\",",
                "\"receipt_signing_key_id\":\"{receipt_key_id}\"}}"
            ),
            root.display(),
            root.display(),
            root.display(),
            root.display(),
            receipts.display(),
            root.display(),
            seed_name = seed_name,
            receipt_key_id = receipt_key_id
        )
        .as_bytes(),
    );
}

fn spawn_paused(fixture: &Fixture, operator: &Path, seam: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_kapsel"))
        .env("KAPSEL_DEMO_CONTROL_DIRECTORY", &fixture.control)
        .env("KAPSEL_DEMO_PAUSE", seam)
        .args([
            "operate",
            "--request",
            fixture.request.to_str().unwrap(),
            "--operator-config",
            operator.to_str().unwrap(),
        ])
        .spawn()
        .unwrap()
}

fn wait_and_kill(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "demo seam marker was not created"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "demo child exited before its marker"
        );
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
}

fn run_operate(fixture: &Fixture, operator: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapsel"))
        .args([
            "operate",
            "--request",
            fixture.request.to_str().unwrap(),
            "--operator-config",
            operator.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

#[test]
fn production_command_recovers_both_demo_process_kill_seams() {
    let mut fixture = fixture();
    let mut mutation = spawn_paused(&fixture, &fixture.operator_a, "after_apply");
    wait_and_kill(&mut mutation, &fixture.control.join("after-apply.ready"));
    assert_eq!(
        fs::read_to_string(fixture.control.join("provider-apply-count")).unwrap(),
        "1"
    );

    let mut publication = spawn_paused(&fixture, &fixture.operator_a, "after_receipt_commit");
    wait_and_kill(
        &mut publication,
        &fixture.control.join("after-receipt-commit.ready"),
    );
    let connection = rusqlite::Connection::open(fixture.root.join("journal.sqlite3")).unwrap();
    let frozen: Vec<u8> = connection
        .query_row(
            "SELECT receipt_bytes FROM kubernetes_image_operations WHERE state = 'finalized'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fs::read_dir(&fixture.receipts_a).unwrap().count(), 0);
    let final_output = run_operate(&fixture, &fixture.operator_b);
    assert_eq!(
        final_output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&final_output.stdout),
        String::from_utf8_lossy(&final_output.stderr)
    );
    assert!(String::from_utf8(final_output.stdout)
        .unwrap()
        .contains("\"state\":\"FINALIZED\""));
    let receipt = fs::read_dir(&fixture.receipts_b)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(&receipt).unwrap(), frozen);
    assert_eq!(
        fs::read_to_string(fixture.control.join("provider-apply-count")).unwrap(),
        "1"
    );

    let inspection = Command::new(env!("CARGO_BIN_EXE_kapsel"))
        .env("KUBECONFIG", "/unavailable/ambient-kubeconfig")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .args([
            "inspect",
            "--receipt",
            receipt.to_str().unwrap(),
            "--trust",
            fixture.trust.to_str().unwrap(),
            "--evaluation-time-unix-s",
            "150",
        ])
        .output()
        .unwrap();
    assert_eq!(inspection.status.code(), Some(0));
    let stdout = String::from_utf8(inspection.stdout).unwrap();
    assert!(stdout.contains("\"status\":\"INSPECTED\""));
    assert!(stdout.contains("\"result\":\"SUCCEEDED\""));
    assert!(!stdout.contains("VERIFIED"));
    let repeated = run_operate(&fixture, &fixture.operator_b);
    assert!(repeated.status.success());
    assert_eq!(fs::read(&receipt).unwrap(), frozen);
    let _ = fixture.stop_server.send(());
    assert_eq!(fixture.server.take().unwrap().join().unwrap(), (3, 1));
}
