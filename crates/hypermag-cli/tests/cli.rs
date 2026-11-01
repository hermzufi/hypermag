use std::collections::BTreeMap;
use std::process::Command;

use hypermag_bencode::Value;

fn sample_torrent() -> Vec<u8> {
    let pieces = [0u8; 40];
    let info = Value::Dict(BTreeMap::from([
        (b"length".as_slice(), Value::Int(5)),
        (b"name".as_slice(), Value::Bytes(b"sample.bin")),
        (b"piece length".as_slice(), Value::Int(4)),
        (b"pieces".as_slice(), Value::Bytes(&pieces)),
    ]));
    let root = Value::Dict(BTreeMap::from([
        (
            b"announce".as_slice(),
            Value::Bytes(b"udp://tracker.example:80"),
        ),
        (b"info".as_slice(), info),
    ]));
    hypermag_bencode::encode(&root)
}

fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("hypermag-{}-{nanos}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn inspect_prints_human_readable_metadata() {
    let path = write_temp("sample.torrent", &sample_torrent());
    let output = Command::new(env!("CARGO_BIN_EXE_hypermag"))
        .arg("inspect")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Name:         sample.bin"), "{stdout}");
    assert!(stdout.contains("Info hash:    "), "{stdout}");
    assert!(stdout.contains("Pieces:       2"), "{stdout}");
    assert!(stdout.contains("udp://tracker.example:80"), "{stdout}");
}

#[test]
fn inspect_emits_valid_json() {
    let path = write_temp("sample.torrent", &sample_torrent());
    let output = Command::new(env!("CARGO_BIN_EXE_hypermag"))
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["name"], "sample.bin");
    assert_eq!(value["piece_count"], 2);
    assert_eq!(value["total_length"], 5);
    assert_eq!(value["trackers"][0], "udp://tracker.example:80");
}

#[test]
fn magnet_prints_info_hash() {
    let hash = "ab".repeat(20);
    let uri = format!("magnet:?xt=urn:btih:{hash}&dn=hello");
    let output = Command::new(env!("CARGO_BIN_EXE_hypermag"))
        .args(["magnet", &uri])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("Info hash v1: {hash}")),
        "{stdout}"
    );
    assert!(stdout.contains("Display name: hello"), "{stdout}");
}
