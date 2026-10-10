//! The shared base prompt teaches agents the Pages workflow (NIP-PG): suggest, don't
//! rewrite; always pass the head you read; exit 5 means re-read. These tests pin the
//! text an agent actually receives, so removing the guidance fails here.

const PROMPT: &str = include_str!("base_prompt.md");

/// The `## Pages` section, without the headings around it.
fn pages_section() -> &'static str {
    let start = PROMPT
        .find("## Pages")
        .expect("the base prompt has a Pages section");
    let rest = &PROMPT[start + "## Pages".len()..];
    let end = rest.find("\n## ").expect("a section follows Pages");
    &rest[..end]
}

#[test]
fn agents_are_told_to_suggest_rather_than_rewrite() {
    let pages = pages_section();
    assert!(pages.contains("Suggest, don't rewrite"));
    assert!(pages.contains("buzz pages suggest <page-id> --channel <uuid> --base <head>"));
    assert!(pages.contains("a person accepts or rejects the suggestion"));
}

#[test]
fn agents_are_told_to_pass_the_head_they_read_and_to_retry_on_exit_5() {
    let pages = pages_section();
    assert!(pages.contains("`<head>` is the `head` you just read"));
    assert!(pages.contains("buzz pages set` (always with `--base <head>`)"));
    assert!(pages.contains("Exit code 5 means the page changed since you read it"));
    assert!(pages.contains("never reuse an old `--base`"));
}

#[test]
fn agents_are_told_not_to_create_pages_unprompted() {
    assert!(pages_section().contains("never create one unprompted"));
}

#[test]
fn the_cli_table_lists_pages_and_the_conflict_exit_code() {
    assert!(PROMPT.contains("| `buzz pages` |"));
    assert!(PROMPT.contains("4 other, 5 write conflict"));
}

#[test]
fn the_pages_section_stays_short() {
    // The base prompt is paid for on every turn of every agent: keep Pages to a
    // paragraph, not a manual (the CLI's --help and the sprout-cli skill hold the rest).
    assert!(
        pages_section().len() < 1_200,
        "Pages section is {} bytes",
        pages_section().len()
    );
}
