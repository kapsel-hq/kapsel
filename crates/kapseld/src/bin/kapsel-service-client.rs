//! Fixed capability-specific client for the Kapsel service.

use std::{
    fs::OpenOptions,
    io::{Read as _, Write as _},
    net::Shutdown,
    os::unix::{
        fs::{OpenOptionsExt as _, PermissionsExt as _},
        net::UnixStream,
    },
    path::Path,
    process::ExitCode,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};

const SOCKET: &str = "/run/kapsel/kapseld.sock";
const RESPONSE_BYTES_MAX: usize = 40 * 1024;
const IO_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadyReceipt {
    status: String,
    receipt_hex: String,
    receipt_sha256: String,
}

#[derive(Serialize)]
struct SavedReceipt<'a> {
    status: &'static str,
    receipt_sha256: &'a str,
    output: &'a str,
}

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(4),
    }
}

fn run(arguments: &[String]) -> Result<(), ()> {
    let (request, output) = request(arguments)?;
    #[cfg(feature = "test-harness")]
    let test_socket = std::env::var("KAPSELD_TEST_CLIENT_SOCKET").ok();
    #[cfg(feature = "test-harness")]
    let socket = test_socket.as_deref().unwrap_or(SOCKET);
    #[cfg(not(feature = "test-harness"))]
    let socket = SOCKET;
    let response = exchange(socket, &request).map_err(|_| ())?;
    match output {
        None => {
            std::io::stdout().write_all(&response).map_err(|_| ())?;
            std::io::stdout().write_all(b"\n").map_err(|_| ())?;
        },
        Some(path) => save_receipt(&response, path)?,
    }
    Ok(())
}

fn request(arguments: &[String]) -> Result<(Vec<u8>, Option<&Path>), ()> {
    match arguments {
        [command, operation_id] if command == "status" => Ok((
            serde_json::to_vec(&json!({
                "request": "get_set_deployment_image_status",
                "operation_id": operation_id,
            }))
            .map_err(|_| ())?,
            None,
        )),
        [command, operation_id, output] if command == "receipt" => Ok((
            serde_json::to_vec(&json!({
                "request": "get_set_deployment_image_receipt",
                "operation_id": operation_id,
            }))
            .map_err(|_| ())?,
            Some(Path::new(output)),
        )),
        [command, operation_id, namespace, deployment, container, image] if command == "submit" => {
            Ok((
                serde_json::to_vec(&json!({
                    "request": "submit_set_deployment_image",
                    "operation_id": operation_id,
                    "namespace": namespace,
                    "deployment": deployment,
                    "container": container,
                    "immutable_image_digest": image,
                }))
                .map_err(|_| ())?,
                None,
            ))
        },
        _ => Err(()),
    }
}

fn exchange(socket: &str, request: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(IO_DEADLINE))?;
    stream.set_write_timeout(Some(IO_DEADLINE))?;
    let length = u32::try_from(request.len())
        .map_err(|_| std::io::Error::other("service request is too large"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(request)?;
    stream.shutdown(Shutdown::Write)?;

    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix)?;
    let length = usize::try_from(u32::from_be_bytes(prefix))
        .map_err(|_| std::io::Error::other("service response is too large"))?;
    if length == 0 || length > RESPONSE_BYTES_MAX {
        return Err(std::io::Error::other("service response is too large"));
    }
    let mut response = vec![0_u8; length];
    stream.read_exact(&mut response)?;
    let mut trailing = [0_u8; 1];
    if stream.read(&mut trailing)? != 0 {
        return Err(std::io::Error::other("service response has trailing bytes"));
    }
    Ok(response)
}

fn save_receipt(response: &[u8], path: &Path) -> Result<(), ()> {
    if !path.is_absolute() {
        return Err(());
    }
    let ready: ReadyReceipt = serde_json::from_slice(response).map_err(|_| ())?;
    if ready.status != "READY" || !lowercase_sha256(&ready.receipt_sha256) {
        return Err(());
    }
    let bytes = decode_lowercase_hex(&ready.receipt_hex)?;
    let expected_digest = decode_lowercase_hex(&ready.receipt_sha256)?;
    if Sha256::digest(&bytes).as_slice() != expected_digest {
        return Err(());
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ())?;
    output
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| ())?;
    if output.metadata().map_err(|_| ())?.permissions().mode() & 0o7777 != 0o600 {
        return Err(());
    }
    output.write_all(&bytes).map_err(|_| ())?;
    output.sync_all().map_err(|_| ())?;
    let path = path.to_str().ok_or(())?;
    let report = serde_json::to_vec(&SavedReceipt {
        status: "READY",
        receipt_sha256: &ready.receipt_sha256,
        output: path,
    })
    .map_err(|_| ())?;
    std::io::stdout().write_all(&report).map_err(|_| ())?;
    std::io::stdout().write_all(b"\n").map_err(|_| ())?;
    Ok(())
}

fn lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn decode_lowercase_hex(value: &str) -> Result<Vec<u8>, ()> {
    if !value.len().is_multiple_of(2)
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0]).ok_or(())?;
            let low = hex_nibble(pair[1]).ok_or(())?;
            Ok((high << 4) | low)
        })
        .collect()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "controlled client fixtures must fail immediately"
)]
mod tests {
    use super::*;

    #[test]
    fn fixed_grammar_has_only_three_capability_commands() {
        let status = vec!["status".into(), "op-1".into()];
        let receipt = vec!["receipt".into(), "op-1".into(), "/tmp/receipt".into()];
        let submit = vec![
            "submit".into(),
            "op-1".into(),
            "demo".into(),
            "agent-api".into(),
            "api".into(),
            concat!(
                "registry.example/agent-api@sha256:",
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .into(),
        ];
        assert!(request(&status).is_ok());
        assert!(request(&receipt).is_ok());
        assert!(request(&submit).is_ok());
        assert!(request(&[]).is_err());
        assert!(request(&["receipt".into(), "op-1".into()]).is_err());
        assert!(request(&["unknown".into(), "op-1".into()]).is_err());
    }

    #[test]
    fn receipt_hex_and_digest_grammar_is_exact() {
        assert_eq!(decode_lowercase_hex("00abff").unwrap(), [0, 0xab, 0xff]);
        assert!(decode_lowercase_hex("0").is_err());
        assert!(decode_lowercase_hex("AB").is_err());
        assert!(decode_lowercase_hex("gg").is_err());
        assert!(lowercase_sha256(&"a".repeat(64)));
        assert!(!lowercase_sha256(&"A".repeat(64)));
    }
}
