use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hypermag_meta::{Magnet, Metainfo};

#[derive(Debug, Parser)]
#[command(
    name = "hypermag",
    version,
    about = "High-throughput BitTorrent engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse a .torrent file and print its metadata.
    Inspect {
        /// Path to a .torrent file.
        path: PathBuf,
        /// List every file in the torrent.
        #[arg(long)]
        files: bool,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Parse a magnet URI and print its info hashes.
    Magnet {
        /// Magnet URI.
        uri: String,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Inspect { path, files, json } => inspect(&path, files, json),
        Command::Magnet { uri, json } => magnet(&uri, json),
    }
}

fn inspect(path: &Path, show_files: bool, json: bool) -> Result<()> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let metainfo =
        Metainfo::parse(&bytes).with_context(|| format!("failed to parse {}", path.display()))?;

    if json {
        print_json(&metainfo)?;
    } else {
        print_human(&metainfo, show_files);
    }
    Ok(())
}

fn print_human(metainfo: &Metainfo<'_>, show_files: bool) {
    let info = &metainfo.info;
    println!("Name:         {}", info.name);
    println!("Info hash:    {} (v1)", metainfo.info_hash_hex());
    println!("Magnet:       {}", metainfo.magnet_link());
    println!(
        "Total size:   {} ({} bytes)",
        human_bytes(info.total_length),
        info.total_length
    );
    println!("Piece length: {}", human_bytes(info.piece_length));
    println!("Pieces:       {}", info.piece_count());
    println!("Private:      {}", if info.private { "yes" } else { "no" });
    if let Some(version) = info.meta_version {
        println!("Meta version: {version}");
    }

    let files = info.file_entries();
    println!("Files:        {}", files.len());
    if show_files {
        for file in &files {
            println!(
                "  {:>10}  {}",
                human_bytes(file.length),
                file.path.join("/")
            );
        }
    }

    let trackers = metainfo.trackers();
    println!("Trackers:     {}", trackers.len());
    for tracker in &trackers {
        println!("  {tracker}");
    }

    if let Some(comment) = &metainfo.comment {
        println!("Comment:      {comment}");
    }
    if let Some(created_by) = &metainfo.created_by {
        println!("Created by:   {created_by}");
    }
    if let Some(date) = metainfo.creation_date {
        println!("Created:      {date} (unix)");
    }
}

fn print_json(metainfo: &Metainfo<'_>) -> Result<()> {
    let files: Vec<_> = metainfo
        .info
        .file_entries()
        .into_iter()
        .map(|file| {
            serde_json::json!({
                "path": file.path.join("/"),
                "length": file.length,
            })
        })
        .collect();

    let value = serde_json::json!({
        "name": &metainfo.info.name,
        "info_hash_v1": metainfo.info_hash_hex(),
        "magnet": metainfo.magnet_link(),
        "total_length": metainfo.info.total_length,
        "piece_length": metainfo.info.piece_length,
        "piece_count": metainfo.info.piece_count(),
        "private": metainfo.info.private,
        "meta_version": metainfo.info.meta_version,
        "trackers": metainfo.trackers(),
        "files": files,
        "comment": &metainfo.comment,
        "created_by": &metainfo.created_by,
        "creation_date": metainfo.creation_date,
    });

    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn magnet(uri: &str, json: bool) -> Result<()> {
    let magnet =
        Magnet::parse(uri).with_context(|| format!("failed to parse magnet URI: {uri}"))?;

    if json {
        let value = serde_json::json!({
            "info_hash_v1": magnet.info_hash_v1_hex(),
            "info_hash_v2": magnet.info_hash_v2_hex(),
            "display_name": &magnet.display_name,
            "trackers": &magnet.trackers,
            "peers": &magnet.peers,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if let Some(hash) = magnet.info_hash_v1_hex() {
        println!("Info hash v1: {}", hash);
    }
    if let Some(hash) = magnet.info_hash_v2_hex() {
        println!("Info hash v2: {}", hash);
    }
    println!(
        "Display name: {}",
        magnet.display_name.as_deref().unwrap_or("-")
    );
    println!("Trackers:     {}", magnet.trackers.len());
    for tracker in &magnet.trackers {
        println!("  {tracker}");
    }
    if !magnet.peers.is_empty() {
        println!("Peers:        {}", magnet.peers.len());
        for peer in &magnet.peers {
            println!("  {peer}");
        }
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn formats_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
    }
}
