//! A socket handler is told which connection asked, and the config hears of
//! a connection that is gone after the last request it sent.

mod common;

use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use serde_json::Value;
use shojiwm_rs::ipc::{ClientId, IpcServer};

thread_local! {
    static IPC: RefCell<Option<IpcServer>> = const { RefCell::new(None) };
    static SEEN: RefCell<Vec<Seen>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Said(ClientId, String),
    Gone(ClientId),
}

fn seen() -> Vec<Seen> {
    SEEN.with(|seen| seen.borrow().clone())
}

fn ipc_path() -> PathBuf {
    std::env::temp_dir().join(format!("shoji-clients-ipc-{}.sock", std::process::id()))
}

fn setup() {
    let ipc = IpcServer::bind(ipc_path()).unwrap();
    ipc.handle_from("say", |client, params| {
        let word = params.as_str().unwrap_or_default().to_owned();
        SEEN.with(|seen| seen.borrow_mut().push(Seen::Said(client, word)));
        Ok(Value::Null)
    });
    ipc.on_disconnect(|client| SEEN.with(|seen| seen.borrow_mut().push(Seen::Gone(client))));
    IPC.with(|slot| *slot.borrow_mut() = Some(ipc));
}

fn connect() -> BufReader<UnixStream> {
    let client = BufReader::new(UnixStream::connect(ipc_path()).unwrap());
    client
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    client
}

/// Says `word` and waits for the answer.
fn say(session: &mut common::Session, client: &mut BufReader<UnixStream>, word: &str) {
    let request = format!("{{\"id\":1,\"method\":\"say\",\"params\":\"{word}\"}}\n");
    client.get_mut().write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    for _ in 0..200 {
        session.roundtrip();
        if client.read_line(&mut reply).is_ok_and(|read| read > 0) {
            break;
        }
    }
    assert!(
        reply.contains("\"result\""),
        "no answer to {word}: {reply:?}"
    );
}

fn said(word: &str) -> ClientId {
    seen()
        .into_iter()
        .find_map(|seen| match seen {
            Seen::Said(client, said) if said == word => Some(client),
            _ => None,
        })
        .unwrap_or_else(|| panic!("nobody said {word}"))
}

#[test]
fn a_handler_knows_its_client_and_the_config_hears_it_leave() {
    let mut session = common::Session::new(setup);
    let mut first = connect();
    let mut second = connect();

    say(&mut session, &mut first, "one");
    say(&mut session, &mut second, "two");
    say(&mut session, &mut first, "three");
    let (a, b) = (said("one"), said("two"));
    assert_ne!(a, b);
    assert_eq!(said("three"), a);

    // A request with no id and the connection closed right behind it: the
    // request is handled, then the connection is reported gone.
    first
        .get_mut()
        .write_all(b"{\"method\":\"say\",\"params\":\"last\"}\n")
        .unwrap();
    drop(first);
    for _ in 0..200 {
        session.roundtrip();
        if seen().contains(&Seen::Gone(a)) {
            break;
        }
    }
    let after: Vec<Seen> = seen().into_iter().skip(3).collect();
    assert_eq!(after, [Seen::Said(a, "last".into()), Seen::Gone(a)]);

    // The other connection is still served, and a new one is nobody's heir.
    say(&mut session, &mut second, "four");
    assert_eq!(said("four"), b);
    let mut third = connect();
    say(&mut session, &mut third, "five");
    assert!(![a, b].contains(&said("five")));

    IPC.with(|slot| slot.borrow_mut().take()).unwrap().close();
    let _ = std::fs::remove_file(ipc_path());
}
