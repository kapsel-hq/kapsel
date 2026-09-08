//! Count real HTTP requests beneath the operator-document application path, including recovery.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "bounded, maintainer-owned loopback fixtures fail the test on invalid evidence"
)]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use ed25519_dalek::SigningKey;
use kapsel::{
    open_application_from_operator_document, provision_exact_grant, AgentRequest, Application,
    ApprovedTarget, ExactAuthorization, GrantProvisioning, OperationResult, OperationState,
    SetDeploymentImageReceipt,
};
use serde_json::{json, Value};

const IMAGE: &str = concat!(
    "example/api@sha256:",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
);

struct WireRequest {
    method: String,
    body: Vec<u8>,
}

struct Fixture {
    root: PathBuf,
    kubeconfig: Vec<u8>,
    stop: mpsc::Sender<()>,
    server: Option<thread::JoinHandle<Vec<WireRequest>>>,
    paused: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}

impl Fixture {
    fn new(status: u16) -> Self {
        Self::with_pause(status, false)
    }

    fn with_pause(status: u16, pause_first_patch: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "kapsel-application-retry-{}-{status}-{pause_first_patch}",
            std::process::id()
        ));
        // Refuse a collision. Never erase another run's retained evidence to make a test pass.
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = mpsc::channel();
        let (pause, paused) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let server = thread::spawn(move || {
            let hold = pause_first_patch.then_some((pause, resumed));
            serve(&listener, &stopped, status, hold)
        });
        let kubeconfig = format!(
            concat!(
                "apiVersion: v1\nkind: Config\nclusters:\n- name: fixture\n",
                "  cluster:\n    server: http://{address}\ncontexts:\n- name: fixture\n",
                "  context:\n    cluster: fixture\n    user: fixture\n",
                "current-context: fixture\nusers:\n- name: fixture\n  user: {{}}\n"
            ),
            address = address
        )
        .into_bytes();
        Self {
            root,
            kubeconfig,
            stop,
            server: Some(server),
            paused,
            resume,
        }
    }

    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "exclusive fixture borrow keeps this future Send despite its non-Sync receiver"
    )]
    async fn application(&mut self) -> Application {
        let request = request();
        let seed = [41; 32];
        let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        let grant = provision_exact_grant(&GrantProvisioning {
            authorization: &ExactAuthorization {
                authorization_id: "retry-approval".into(),
                operation_id: request.operation_id,
                namespace: request.namespace,
                deployment: request.deployment,
                container: request.container,
                immutable_image_digest: request.immutable_image_digest,
                approved_target: Some(ApprovedTarget {
                    uid: "uid-1".into(),
                    resource_version: "1".into(),
                }),
            },
            signing_seed: &seed,
            signing_key_id: "retry-authority",
        })
        .unwrap();
        let document = json!({
            "signed_authorization_grant": self.root.join("grant"),
            "authorization_key_id": "retry-authority",
            "authorization_public_key": self.root.join("public-key"),
            "kubeconfig": self.root.join("kubeconfig"),
            "journal": self.root.join("journal.sqlite3"),
            "receipt_signing_seed": self.root.join("receipt-seed"),
            "receipt_signing_key_id": "retry-receipt"
        });
        open_application_from_operator_document(
            &serde_json::to_vec(&document).unwrap(),
            |path: &Path, _| {
                // Operator-owned fixture reader. No ambient files, trust, or caller authority.
                let bytes = match path.file_name().unwrap().to_str().unwrap() {
                    "grant" => grant.clone(),
                    "public-key" => public_key.to_vec(),
                    "kubeconfig" => self.kubeconfig.clone(),
                    "receipt-seed" => vec![42; 32],
                    unexpected => panic!("unexpected operator read: {unexpected}"),
                };
                Ok(bytes)
            },
        )
        .await
        .unwrap()
    }

    fn finish(&mut self) -> Vec<WireRequest> {
        self.stop.send(()).unwrap();
        self.server.take().unwrap().join().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.resume.send(());
        let _ = self.stop.send(());
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request() -> AgentRequest {
    AgentRequest {
        operation_id: "retry-op".into(),
        namespace: "demo".into(),
        deployment: "api".into(),
        container: "api".into(),
        immutable_image_digest: IMAGE.into(),
    }
}

fn deployment(changed: bool) -> Value {
    let generation = if changed { 2 } else { 1 };
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {
            "name": "api", "namespace": "demo", "uid": "uid-1",
            "resourceVersion": if changed { "2" } else { "1" }, "generation": generation,
            "annotations": {
                "kapsel.dev/kap0038-operation-id": if changed { "retry-op" } else { "" }
            }
        },
        "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "api"}},
            "template": {"spec": {"containers": [{"name": "api", "image":
                if changed { IMAGE } else { "example/api:old" }}]}}},
        "status": {"observedGeneration": generation, "updatedReplicas": 1,
            "availableReplicas": 1, "unavailableReplicas": 0,
            "conditions": [{"type": "Available", "status": "True",
                "reason": "MinimumReplicasAvailable"}]}
    })
}

fn serve(
    listener: &TcpListener,
    stopped: &mpsc::Receiver<()>,
    status: u16,
    mut hold: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
) -> Vec<WireRequest> {
    let mut requests = Vec::new();
    let mut patches = 0;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stopped.try_recv().is_ok() {
                    return requests;
                }
                assert!(Instant::now() < deadline, "loopback receiver timed out");
                thread::sleep(Duration::from_millis(1));
                continue;
            },
            Err(error) => panic!("receiver accept: {error}"),
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = read_request(&mut stream);
        let code = match request.method.as_str() {
            "GET" => 200,
            "PATCH" => {
                patches += 1;
                // The first request acts, but its response is ambiguous. A repeated stale request
                // conflicts. Count it anyway: absence of a second stored change is not no-replay.
                if patches == 1 {
                    status
                } else {
                    409
                }
            },
            method => panic!("unexpected method: {method}"),
        };
        requests.push(request);
        if patches == 1 {
            if let Some((pause, resumed)) = hold.take() {
                pause.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }
        assert!(requests.len() <= 8, "unbounded client requests");
        if code == 0 {
            // Complete request received, response connection lost.
            continue;
        }
        let body = if code == 200 {
            deployment(patches > 0)
        } else {
            json!({"apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "FixtureAmbiguity", "code": code})
        };
        let body = serde_json::to_vec(&body).unwrap();
        write!(
            stream,
            concat!(
                "HTTP/1.1 {code} Fixture\r\ncontent-type: application/json\r\n",
                "content-length: {}\r\nconnection: close\r\n\r\n"
            ),
            body.len(),
            code = code
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> WireRequest {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = match stream.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.unwrap(),
        };
        assert!(count > 0, "incomplete request");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 16 * 1024);
        let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let end = end + 4;
        let headers = std::str::from_utf8(&bytes[..end]).unwrap();
        let length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, length)| length.trim().parse::<usize>().unwrap());
        assert!(length <= 16 * 1024);
        if bytes.len() >= end + length {
            assert_eq!(bytes.len(), end + length);
            return WireRequest {
                method: headers.split_ascii_whitespace().next().unwrap().into(),
                body: bytes[end..].to_vec(),
            };
        }
    }
}

#[tokio::test]
async fn healthy_dispatch_and_restart_preserve_one_http_request_and_original_receipt() {
    let mut fixture = Fixture::new(200);
    let mut application = fixture.application().await;
    let report = application.execute(&request()).await.unwrap();
    assert_eq!(report.result, Some(OperationResult::Succeeded));
    let original = application
        .read_set_deployment_image_receipt("retry-op")
        .unwrap();
    drop(application);
    let mut application = fixture.application().await;
    assert_eq!(application.execute(&request()).await.unwrap(), report);
    assert_eq!(
        application
            .read_set_deployment_image_receipt("retry-op")
            .unwrap(),
        original
    );
    drop(application);
    let requests = fixture.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PATCH")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        2
    );
}

#[tokio::test]
async fn cancelled_application_dispatch_recovers_without_resending_to_available_receiver() {
    let mut fixture = Fixture::with_pause(0, true);
    let mut application = fixture.application().await;
    let requested = request();
    let mut execution = Box::pin(application.execute(&requested));
    tokio::select! {
        result = &mut execution => panic!("response must still be pending: {result:?}"),
        () = async {
            let deadline = Instant::now() + Duration::from_secs(5);
            while fixture.paused.try_recv().is_err() {
                assert!(Instant::now() < deadline, "PATCH not received");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        } => {}
    }
    drop(execution);
    drop(application);
    fixture.resume.send(()).unwrap();
    let mut application = fixture.application().await;
    let report = application.reconcile().await.unwrap().unwrap();
    assert_eq!(report.result, Some(OperationResult::Succeeded));
    let original = application
        .read_set_deployment_image_receipt("retry-op")
        .unwrap();
    assert_eq!(application.execute(&request()).await.unwrap(), report);
    assert_eq!(
        application
            .read_set_deployment_image_receipt("retry-op")
            .unwrap(),
        original
    );
    drop(application);
    let requests = fixture.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "PATCH")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET")
            .count(),
        2
    );
}

#[tokio::test]
async fn ambiguous_patch_responses_never_trigger_hidden_client_retries() {
    let mut counts = Vec::new();
    for status in [429, 503, 504, 0] {
        let mut fixture = Fixture::new(status);
        let mut application = fixture.application().await;
        assert!(application.execute(&request()).await.is_err());
        drop(application);
        let mut application = fixture.application().await;
        let report = application.reconcile().await.unwrap().unwrap();
        assert_eq!(report.state, OperationState::Finalized);
        assert_eq!(report.result, Some(OperationResult::Succeeded));
        assert_eq!(
            report.targets.attempt_target,
            report.targets.approved_target
        );
        let original = application
            .read_set_deployment_image_receipt("retry-op")
            .unwrap();
        assert!(matches!(original, SetDeploymentImageReceipt::Ready { .. }));
        assert_eq!(application.execute(&request()).await.unwrap(), report);
        assert_eq!(
            application
                .read_set_deployment_image_receipt("retry-op")
                .unwrap(),
            original
        );
        drop(application);
        let requests = fixture.finish();
        let patches: Vec<_> = requests
            .iter()
            .filter(|request| request.method == "PATCH")
            .collect();
        for patch in &patches {
            let body: Value = serde_json::from_slice(&patch.body).unwrap();
            assert_eq!(body["metadata"]["uid"], "uid-1");
            assert_eq!(body["metadata"]["resourceVersion"], "1");
            assert_eq!(
                body["spec"]["template"]["spec"]["containers"][0]["image"],
                IMAGE
            );
        }
        counts.push((status, patches.len()));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "GET")
                .count(),
            2
        );
    }
    assert_eq!(counts, [(429, 1), (503, 1), (504, 1), (0, 1)]);
}
