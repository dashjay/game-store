//! GameStore Phase-1 spike (Rust).
//!
//! A minimal RESP2 server backed by RocksDB that demonstrates the
//! `docs/design/03-storage-engine.md` encoding (metadata + subkey + version)
//! and version-based subkey GC via a RocksDB compaction filter.
//!
//! Usage: gamestore-spike [--port N] [--db PATH]

mod commands;
mod encoding;
mod gc;
mod resp;
mod storage;

use std::io::{BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use storage::Store;

fn main() {
    let mut port: u16 = 6380;
    let mut db_path = "/tmp/gamestore-spike-rust".to_string();

    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--port" if i + 1 < argv.len() => {
                port = argv[i + 1].parse().expect("invalid --port");
                i += 2;
            }
            "--db" if i + 1 < argv.len() => {
                db_path = argv[i + 1].clone();
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {}", other);
                i += 1;
            }
        }
    }

    let store = Arc::new(Store::open(&db_path).expect("open store"));
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    println!(
        "[rust] GameStore spike listening on 127.0.0.1:{} (db={})",
        port, db_path
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    if let Err(e) = handle_conn(stream, store) {
                        eprintln!("[rust] connection error: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("[rust] accept error: {}", e),
        }
    }
}

fn handle_conn(stream: TcpStream, store: Arc<Store>) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let read_stream = stream.try_clone()?;
    let mut reader = BufReader::new(read_stream);
    let mut writer = BufWriter::new(stream);

    loop {
        match resp::read_command(&mut reader)? {
            None => break, // EOF
            Some(args) if args.is_empty() => continue,
            Some(args) => {
                let (reply, close) = commands::dispatch(&store, &args);
                reply.write_to(&mut writer)?;
                writer.flush()?;
                if close {
                    break;
                }
            }
        }
    }
    Ok(())
}
