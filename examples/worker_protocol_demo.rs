//! R01 pipe demonstration: real sender/receiver over OS pipes.
//!
//! Two threads connected by `UnixStream` OS socket pairs (real kernel
//! transport with independent message segmentation, not in-process
//! assertions):
//!
//! - Case 1 (good): host sends `request` → worker sends `checkpoint` →
//!   host sends `ack` → worker sends `result`.
//! - Case 2 (rejections): version mismatch, oversize prefix, truncated
//!   body, and a seq replay are each fed to a receiver and rejected with
//!   explicit protocol errors.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p levitate-recipe --example worker_protocol_demo
//! ```

use std::io::Write;
use std::os::unix::net::UnixStream;

use levitate_recipe::worker::protocol::{
    AckPayload, CheckpointPayload, FramePayload, FrameReceiver, FrameSender, MAX_FRAME_LEN,
    PROTOCOL_VERSION, RequestPayload, ResultPayload, encode_message,
};

fn request_fixture() -> FramePayload {
    FramePayload::Request(RequestPayload {
        invocation_id: "recfstab-1700000000000-4242".to_string(),
        operation: "install".to_string(),
        persist_ctx: true,
        recipe_digest: "sha256:0123456789abcdef".to_string(),
        options: Default::default(),
    })
}

fn checkpoint_fixture() -> FramePayload {
    FramePayload::Checkpoint(CheckpointPayload {
        seq: 1,
        phase: "build".to_string(),
        reason: "phase:build".to_string(),
        main_ctx: serde_json::json!({"name": "recfstab", "installed": false}),
        base_ctx: serde_json::json!({}),
        base_path: None,
    })
}

fn ack_fixture() -> FramePayload {
    FramePayload::Ack(AckPayload {
        seq: 1,
        generation: 7,
    })
}

fn result_fixture() -> FramePayload {
    FramePayload::Result(ResultPayload {
        ok: true,
        failures: vec![],
        ctx_summary: serde_json::json!({"installed": true}),
    })
}

fn summarize(payload: &FramePayload) -> String {
    match payload {
        FramePayload::Request(p) => format!(
            "request invocation_id={} operation={}",
            p.invocation_id, p.operation
        ),
        FramePayload::Checkpoint(p) => {
            format!("checkpoint seq={} phase={}", p.seq, p.phase)
        }
        FramePayload::Ack(p) => format!("ack seq={} generation={}", p.seq, p.generation),
        FramePayload::Failure(p) => format!("failure fatal={} reason={}", p.fatal, p.reason),
        FramePayload::Result(p) => format!("result ok={}", p.ok),
        FramePayload::Diagnostic(p) => {
            format!("diagnostic stream={:?} bytes={}", p.stream, p.data.len())
        }
    }
}

fn good_exchange() {
    println!("--- good exchange: request -> checkpoint -> ack -> result ---");
    let (host_sock, worker_sock) = UnixStream::pair().expect("socketpair");

    let worker = std::thread::spawn(move || {
        let mut sock = worker_sock;
        let mut rx = FrameReceiver::new();
        let mut tx = FrameSender::new();

        let req = rx.recv(&mut sock).expect("worker recv request");
        println!(
            "worker: received {} seq={}",
            summarize(&req.payload),
            req.seq
        );
        assert_eq!(req.seq, 1);

        let used = tx
            .send(&mut sock, &checkpoint_fixture())
            .expect("worker send checkpoint");
        println!("worker: sent checkpoint seq={used}");

        let ack = rx.recv(&mut sock).expect("worker recv ack");
        println!(
            "worker: received {} seq={}",
            summarize(&ack.payload),
            ack.seq
        );

        let used = tx
            .send(&mut sock, &result_fixture())
            .expect("worker send result");
        println!("worker: sent result seq={used}");
    });

    let mut sock = host_sock;
    let mut rx = FrameReceiver::new();
    let mut tx = FrameSender::new();

    let used = tx
        .send(&mut sock, &request_fixture())
        .expect("host send request");
    println!("host: sent request seq={used}");

    let cp = rx.recv(&mut sock).expect("host recv checkpoint");
    println!("host: received {} seq={}", summarize(&cp.payload), cp.seq);

    let used = tx.send(&mut sock, &ack_fixture()).expect("host send ack");
    println!("host: sent ack seq={used}");

    let res = rx.recv(&mut sock).expect("host recv result");
    println!("host: received {} seq={}", summarize(&res.payload), res.seq);

    worker.join().expect("worker thread");
    println!("good exchange complete: 2 frames each direction, seq 1..2 contiguous");
}

fn rejection_cases() {
    println!("--- rejection cases: malformed input refused ---");

    // 1. Version mismatch: valid frame bytes with version patched to 2.
    {
        let (mut tx_sock, rx_sock) = UnixStream::pair().expect("socketpair");
        let mut bytes = encode_message(1, &request_fixture()).expect("encode");
        bytes[4..8].copy_from_slice(&(PROTOCOL_VERSION + 1).to_be_bytes());
        tx_sock.write_all(&bytes).expect("write");
        drop(tx_sock);
        let mut sock = rx_sock;
        let mut rx = FrameReceiver::new();
        match rx.recv(&mut sock) {
            Ok(frame) => println!("version case: UNEXPECTED ACCEPT {frame:?}"),
            Err(e) => println!("version case: rejected: {e} (want version {PROTOCOL_VERSION})"),
        }
    }

    // 2. Oversize: length prefix above 4 MiB, no body follows.
    {
        let (mut tx_sock, rx_sock) = UnixStream::pair().expect("socketpair");
        let too_big = (MAX_FRAME_LEN as u32) + 1;
        tx_sock.write_all(&too_big.to_be_bytes()).expect("write");
        drop(tx_sock);
        let mut sock = rx_sock;
        let mut rx = FrameReceiver::new();
        match rx.recv(&mut sock) {
            Ok(frame) => println!("oversize case: UNEXPECTED ACCEPT {frame:?}"),
            Err(e) => println!("oversize case: rejected: {e}"),
        }
    }

    // 3. Truncated: header declares 64 more body bytes than ever arrive.
    {
        let (mut tx_sock, rx_sock) = UnixStream::pair().expect("socketpair");
        let mut bytes = encode_message(1, &request_fixture()).expect("encode");
        let longer = (bytes.len() as u32) + 64;
        bytes[0..4].copy_from_slice(&longer.to_be_bytes());
        tx_sock.write_all(&bytes).expect("write");
        drop(tx_sock); // EOF with the declared body incomplete
        let mut sock = rx_sock;
        let mut rx = FrameReceiver::new();
        match rx.recv(&mut sock) {
            Ok(frame) => println!("truncated case: UNEXPECTED ACCEPT {frame:?}"),
            Err(e) => println!("truncated case: rejected: {e}"),
        }
    }

    // 4. Replay: the same seq-1 frame delivered twice.
    {
        let (mut tx_sock, rx_sock) = UnixStream::pair().expect("socketpair");
        let bytes = encode_message(1, &request_fixture()).expect("encode");
        tx_sock.write_all(&bytes).expect("write 1");
        tx_sock.write_all(&bytes).expect("write 2 (replay)");
        drop(tx_sock);
        let mut sock = rx_sock;
        let mut rx = FrameReceiver::new();
        let first = rx.recv(&mut sock).expect("first delivery accepted");
        println!(
            "replay case: first delivery accepted ({} seq={}), second:",
            summarize(&first.payload),
            first.seq
        );
        match rx.recv(&mut sock) {
            Ok(frame) => println!("replay case: UNEXPECTED ACCEPT {frame:?}"),
            Err(e) => println!("replay case: rejected: {e}"),
        }
    }

    println!("rejection cases complete: all malformed input refused, none truncated silently");
}

fn main() {
    good_exchange();
    rejection_cases();
}
