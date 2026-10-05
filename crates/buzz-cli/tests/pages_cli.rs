//! `buzz pages` driven through the production `buzz` binary.
//!
//! Two kinds of test live here:
//!
//! * Offline tests run the binary against an unreachable relay and prove that a
//!   command is refused *before* it touches the network (they would otherwise exit 2).
//! * `#[ignore]` live tests run the whole workflow against a real relay, the same
//!   path an agent takes: exit codes, stdout JSON and the relay's own conflict checks.
//!   Start a relay (`just relay`, or `scripts/start-relay-for-tests.sh`), then:
//!
//!   ```text
//!   RELAY_URL=ws://localhost:3000 cargo test -p buzz-cli --test pages_cli -- --ignored
//!   ```

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_buzz");
/// A port nothing listens on: any command that reaches the network fails with exit 2.
const UNREACHABLE: &str = "http://127.0.0.1:9";
const CHANNEL: &str = "11111111-1111-4111-8111-111111111111";
const PAGE: &str = "22222222-2222-4222-8222-222222222222";

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// stdout parsed as JSON.
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", self.stdout))
    }

    fn field(&self, name: &str) -> String {
        self.json()[name]
            .as_str()
            .unwrap_or_else(|| panic!("no string field {name:?} in {}", self.stdout))
            .to_owned()
    }
}

/// A `buzz` identity talking to one relay.
struct Cli {
    relay: String,
    key: String,
}

impl Cli {
    fn new(relay: &str) -> Self {
        Self {
            relay: relay.to_owned(),
            key: nostr::Keys::generate().secret_key().to_secret_hex(),
        }
    }

    fn run_with_stdin(&self, args: &[&str], stdin: Option<&str>) -> Run {
        let mut child = Command::new(BIN)
            .env("BUZZ_RELAY_URL", &self.relay)
            .env("BUZZ_PRIVATE_KEY", &self.key)
            .env_remove("BUZZ_AUTH_TAG")
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn buzz");
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write stdin");
        }
        let out = child.wait_with_output().expect("wait for buzz");
        Run {
            code: out.status.code().expect("exit code"),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_with_stdin(args, None)
    }
}

// -- Offline: refused before the network ----------------------------------------------------

#[test]
fn an_edit_without_base_exits_1_before_reaching_the_relay() {
    let cli = Cli::new(UNREACHABLE);
    let run = cli.run(&[
        "pages",
        "set",
        "--channel",
        CHANNEL,
        "--page",
        PAGE,
        "--title",
        "T",
        "--content",
        "x",
    ]);
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("--base"), "stderr: {}", run.stderr);
}

#[test]
fn set_needs_either_a_page_or_new() {
    let cli = Cli::new(UNREACHABLE);
    let run = cli.run(&["pages", "set", "--channel", CHANNEL, "--content", "x"]);
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
}

#[test]
fn oversize_stdin_is_refused_before_the_network() {
    let cli = Cli::new(UNREACHABLE);
    let big = "a".repeat(65_537);
    let run = cli.run_with_stdin(
        &[
            "pages",
            "set",
            "--channel",
            CHANNEL,
            "--new",
            "--title",
            "T",
        ],
        Some(&big),
    );
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("65536"), "stderr: {}", run.stderr);
}

#[test]
fn a_reachable_failure_is_not_an_input_error() {
    // Control for the tests above: with valid input the same binary does try the
    // network, so exit 1 there really means "refused locally".
    let cli = Cli::new(UNREACHABLE);
    let run = cli.run(&["pages", "get", PAGE, "--channel", CHANNEL]);
    assert_ne!(run.code, 1, "stderr: {}", run.stderr);
}

// -- Live: the whole workflow against a relay -----------------------------------------------------

fn relay_url() -> String {
    std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://localhost:3000".to_owned())
}

fn ok(run: Run) -> Run {
    assert_eq!(
        run.code, 0,
        "stderr: {}\nstdout: {}",
        run.stderr, run.stdout
    );
    run
}

fn ids(run: &Run) -> Vec<String> {
    run.json()
        .as_array()
        .expect("array")
        .iter()
        .map(|e| e["id"].as_str().expect("id").to_owned())
        .collect()
}

#[test]
#[ignore = "needs a running relay (RELAY_URL)"]
fn pages_workflow_against_a_live_relay() {
    let human = Cli::new(&relay_url());
    let agent = Cli::new(&relay_url());
    let name = format!("pages-cli-{}", uuid::Uuid::new_v4().simple());
    let channel = ok(human.run(&[
        "channels",
        "create",
        "--name",
        &name,
        "--type",
        "stream",
        "--visibility",
        "open",
    ]))
    .field("channel_id");
    let ch = channel.as_str();

    // create
    let created = ok(human.run_with_stdin(
        &[
            "pages",
            "set",
            "--channel",
            ch,
            "--new",
            "--title",
            "Runbook",
        ],
        Some("# Runbook\n\nStep 1\n"),
    ));
    let page = created.field("page_id");
    let rev1 = created.field("event_id");
    let page = page.as_str();

    // get: head, title and author
    let got = ok(human.run(&["pages", "get", page, "--channel", ch])).json();
    assert_eq!(got["head"], rev1.as_str());
    assert_eq!(got["title"], "Runbook");
    assert_eq!(got["content"], "# Runbook\n\nStep 1\n");

    // edit on the head
    let rev2 = ok(human.run(&[
        "pages",
        "set",
        "--channel",
        ch,
        "--page",
        page,
        "--base",
        &rev1,
        "--content",
        "# Runbook\n\nStep 1\nStep 2\n",
    ]))
    .field("event_id");

    // an edit on the now-stale base: exit 5, and the error names the head to retry on
    let stale = human.run(&[
        "pages",
        "set",
        "--channel",
        ch,
        "--page",
        page,
        "--base",
        &rev1,
        "--content",
        "lost",
    ]);
    assert_eq!(stale.code, 5, "stderr: {}", stale.stderr);
    assert!(stale.stderr.contains(&rev2), "stderr: {}", stale.stderr);
    let head_after_conflict = ok(human.run(&["pages", "get", page, "--channel", ch])).field("head");
    assert_eq!(head_after_conflict, rev2, "a rejected edit changes nothing");

    // retry on the fresh head
    let rev3 = ok(human.run(&[
        "pages",
        "set",
        "--channel",
        ch,
        "--page",
        page,
        "--base",
        &rev2,
        "--content",
        "# Runbook\n\nStep 1\nStep 2\nStep 3\n",
    ]))
    .field("event_id");

    // agent suggests on the head; the page does not change
    let sugg = ok(agent.run_with_stdin(
        &["pages", "suggest", page, "--channel", ch, "--base", &rev3],
        Some("# Runbook\n\nStep 1\nStep 2\nStep 3\nStep 4\n"),
    ))
    .field("suggestion_id");
    assert_eq!(
        ok(human.run(&["pages", "get", page, "--channel", ch])).field("head"),
        rev3
    );
    let open = ok(human.run(&[
        "--format",
        "compact",
        "pages",
        "history",
        page,
        "--channel",
        ch,
        "--suggestions",
    ]));
    assert!(ids(&open).contains(&sugg), "an open suggestion is listed");

    // a suggestion on an older base goes stale once the head moves
    let stale_sugg = ok(agent.run(&[
        "pages",
        "suggest",
        page,
        "--channel",
        ch,
        "--base",
        &rev2,
        "--content",
        "from rev2",
    ]))
    .field("suggestion_id");

    // accept: one revision, the head advances to it
    let revisions_before = ids(&ok(human.run(&[
        "--format",
        "compact",
        "pages",
        "history",
        page,
        "--channel",
        ch,
    ])))
    .len();
    let applied = ok(human.run(&["pages", "accept", &sugg, "--channel", ch, "--page", page]))
        .field("event_id");
    let head = ok(human.run(&["pages", "get", page, "--channel", ch])).json();
    assert_eq!(head["head"], applied.as_str());
    assert_eq!(head["prev"], rev3.as_str());
    assert_eq!(
        head["content"],
        "# Runbook\n\nStep 1\nStep 2\nStep 3\nStep 4\n"
    );
    let history = ok(human.run(&[
        "--format",
        "compact",
        "pages",
        "history",
        page,
        "--channel",
        ch,
        "--suggestions",
    ]));
    let listed = ids(&history);
    assert_eq!(
        listed
            .iter()
            .filter(|id| *id != &sugg && *id != &stale_sugg)
            .count(),
        revisions_before + 1,
        "accepting adds exactly one revision"
    );
    assert!(!listed.contains(&sugg), "an applied suggestion is closed");
    let applying = history
        .json()
        .as_array()
        .expect("array")
        .iter()
        .find(|e| e["id"] == applied.as_str())
        .expect("the applied revision is listed")
        .clone();
    assert_eq!(
        applying["suggestion"],
        sugg.as_str(),
        "the revision carries the suggestion tag"
    );

    // accepting the stale suggestion is refused (exit 5) and publishes nothing
    let refused = human.run(&[
        "pages",
        "accept",
        &stale_sugg,
        "--channel",
        ch,
        "--page",
        page,
    ]);
    assert_eq!(refused.code, 5, "stderr: {}", refused.stderr);
    assert!(
        refused.stderr.contains("stale"),
        "stderr: {}",
        refused.stderr
    );
    assert_eq!(
        ok(human.run(&["pages", "get", page, "--channel", ch])).field("head"),
        applied,
        "a refused accept leaves the head alone"
    );

    // reject closes it
    ok(human.run(&[
        "pages",
        "reject",
        &stale_sugg,
        "--channel",
        ch,
        "--page",
        page,
    ]));
    let after = ok(human.run(&[
        "--format",
        "compact",
        "pages",
        "history",
        page,
        "--channel",
        ch,
        "--suggestions",
    ]));
    assert!(
        !ids(&after).contains(&stale_sugg),
        "a rejected suggestion is closed"
    );

    // history: newest first, deduplicated
    let revisions = ids(&ok(human.run(&[
        "--format",
        "compact",
        "pages",
        "history",
        page,
        "--channel",
        ch,
    ])));
    assert_eq!(revisions, vec![applied.clone(), rev3, rev2, rev1]);

    // export: byte for byte
    let exported = ok(human.run(&["pages", "export", page, "--channel", ch]));
    assert_eq!(
        exported.stdout,
        "# Runbook\n\nStep 1\nStep 2\nStep 3\nStep 4\n"
    );

    // ls: one entry for this channel, pointing at the head
    let library = ok(human.run(&["pages", "ls", "--channel", ch])).json();
    assert_eq!(library.as_array().expect("array").len(), 1);
    assert_eq!(library[0]["page_id"], page);
    assert_eq!(library[0]["head"], applied.as_str());

    // a no-op edit is the relay's `invalid:` (exit 1)
    let noop = human.run(&[
        "pages",
        "set",
        "--channel",
        ch,
        "--page",
        page,
        "--base",
        &applied,
        "--content",
        "# Runbook\n\nStep 1\nStep 2\nStep 3\nStep 4\n",
    ]);
    assert_eq!(noop.code, 1, "stderr: {}", noop.stderr);
    assert!(noop.stderr.contains("no-op"), "stderr: {}", noop.stderr);
}
