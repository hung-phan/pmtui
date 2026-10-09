//! Directory completion for the create form's Directory row: the ghost tail it would append, the
//! candidate list it would show, and the tilde rules — all against a scratch tree and a scratch
//! `$HOME`, never the machine's, so the suite cannot depend on (or disturb) the developer's home.

use crate::path_complete::{complete_dir, complete_dir_in};

/// A scratch tree holding the children each test names.
fn tree(children: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for c in children {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    d
}

fn tail(input: &str) -> Option<String> {
    complete_dir(input).tail
}

fn options(input: &str) -> Vec<String> {
    complete_dir(input).options
}

/// Nothing to offer: no ghost and no list. Asserted on those two rather than on the whole value,
/// because `base` names the directory that was READ, which is meaningful even when it held no
/// candidates — and is never used without an option index.
fn offers_nothing(input: &str) {
    let got = complete_dir(input);
    assert_eq!(got.tail, None, "tail for {input:?}");
    assert!(got.options.is_empty(), "options for {input:?}: {got:?}");
}

#[test]
fn a_unique_directory_completes_and_offers_the_separator() {
    let d = tree(&["project-alpha"]);
    let typed = format!("{}/proj", d.path().display());
    // The separator comes with it, so the next keystroke descends instead of re-typing `/`.
    assert_eq!(
        tail(&typed).as_deref(),
        Some("ect-alpha/"),
        "a single match completes the whole name plus the separator"
    );
    assert_eq!(options(&typed), ["project-alpha"]);
}

#[test]
fn ambiguous_candidates_complete_only_the_shared_prefix_and_list_them_all() {
    let d = tree(&["project-alpha", "project-beta"]);
    let typed = format!("{}/pro", d.path().display());
    // `project-` is not a directory, so appending `/` would complete to a path that cannot exist —
    // and the list is what answers "which one did you mean".
    assert_eq!(tail(&typed).as_deref(), Some("ject-"));
    assert_eq!(options(&typed), ["project-alpha", "project-beta"]);
}

#[test]
fn a_path_that_already_names_a_directory_lists_what_is_inside_it() {
    let d = tree(&["project-alpha/inner", "project-alpha/other"]);
    let typed = format!("{}/project-alpha", d.path().display());
    // The question the list answers is "where can I go from here", so naming a directory shows its
    // children — not that directory's own name handed back.
    assert_eq!(options(&typed), ["inner", "other"]);
    // And the only completion left is the separator, which is deliberately NOT offered: every
    // freshly opened form holds a path like this, and a ghost here would spend the `Tab` that was
    // about to leave the row.
    assert_eq!(tail(&typed), None);
    // Typing the separator is what turns the children into real completions.
    let typed = format!("{typed}/");
    assert_eq!(options(&typed), ["inner", "other"]);
    assert_eq!(tail(&typed), None, "`inner` and `other` share no prefix");
}

#[test]
fn a_trailing_separator_completes_when_the_children_share_a_prefix() {
    let one = tree(&["sole"]);
    assert_eq!(
        tail(&format!("{}/", one.path().display())).as_deref(),
        Some("sole/")
    );
    let two = tree(&["a-one", "b-two"]);
    assert_eq!(
        tail(&format!("{}/", two.path().display())),
        None,
        "an empty shared prefix is no suggestion, not an empty one"
    );
}

#[test]
fn files_are_never_candidates() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("notes.md"), b"x").unwrap();
    // The field names a project ROOT: completing to a file would only produce a submit the form
    // has to refuse.
    let typed = format!("{}/not", d.path().display());
    assert_eq!(tail(&typed), None);
    assert!(options(&typed).is_empty());
}

#[test]
fn a_symlink_to_a_directory_is_a_candidate() {
    let target = tree(&["real-target"]);
    let d = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(target.path().join("real-target"), d.path().join("linked")).unwrap();
    // `file_type` reports the LINK, so this is the case that needs the `metadata` fallback.
    assert_eq!(
        tail(&format!("{}/link", d.path().display())).as_deref(),
        Some("ed/")
    );
}

#[test]
fn a_symlink_to_a_file_is_not_a_candidate() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("target.txt");
    std::fs::write(&file, b"x").unwrap();
    std::os::unix::fs::symlink(&file, d.path().join("linked")).unwrap();
    assert_eq!(tail(&format!("{}/link", d.path().display())), None);
}

#[test]
fn a_dotfile_surfaces_only_once_the_dot_is_typed() {
    let d = tree(&[".hidden", "visible"]);
    let root = d.path().display().to_string();
    // Bare: only the visible sibling is a candidate, so the completion is unambiguous and the list
    // stays free of the noise nobody asked for.
    assert_eq!(tail(&format!("{root}/")).as_deref(), Some("visible/"));
    assert_eq!(options(&format!("{root}/")), ["visible"]);
    // Typing the dot is the human saying they want one.
    assert_eq!(tail(&format!("{root}/.")).as_deref(), Some("hidden/"));
    assert_eq!(options(&format!("{root}/.")), [".hidden"]);
}

#[test]
fn nothing_to_offer_is_an_empty_completion() {
    let d = tree(&["project"]);
    let root = d.path().display().to_string();
    // Empty input, a directory that does not exist, and a fragment nothing starts with all mean
    // the same thing to the key handler: there is no ghost, so `Tab` must move to the next field.
    for input in ["", &format!("{root}/nope/deeper"), &format!("{root}/zzz")] {
        offers_nothing(input);
    }
    // A bare word has no separator to split on. It is also NOT resolved against the process
    // working directory, which is not a place this field ever meant.
    offers_nothing("project");
}

#[test]
fn the_candidate_list_is_bounded() {
    let names: Vec<String> = (0..70).map(|i| format!("dir-{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let d = tree(&refs);
    // Past the cap the answer is "keep typing", so the list stops rather than holding every name
    // of a huge directory on every keystroke. Sorted, so the cut is the same cut every time.
    let got = options(&format!("{}/", d.path().display()));
    assert_eq!(got.len(), 64);
    assert_eq!(got[0], "dir-000");
}

#[test]
fn a_tilde_resolves_against_the_injected_home() {
    let home = tree(&["workspace"]);
    assert_eq!(
        complete_dir_in("~/w", home.path()).tail.as_deref(),
        Some("orkspace/")
    );
    assert_eq!(
        complete_dir_in("~/", home.path()).tail.as_deref(),
        Some("workspace/")
    );
    // A bare `~` names the home directory itself, so it lists what is inside it.
    assert_eq!(complete_dir_in("~", home.path()).options, ["workspace"]);
    assert_eq!(complete_dir_in("~", home.path()).tail, None);
    // `~other` is somebody else's home, which this must never silently read as the current user's.
    let other = complete_dir_in("~other/w", home.path());
    assert_eq!(other.tail, None);
    assert!(other.options.is_empty(), "{other:?}");
}

#[test]
fn a_candidate_knows_the_whole_path_it_would_become() {
    let d = tree(&["project-alpha"]);
    let root = d.path().display().to_string();
    // While a fragment is being typed, the candidates live in its PARENT.
    let typing = complete_dir(&format!("{root}/pro"));
    assert_eq!(
        typing.option_path(0).as_deref(),
        Some(format!("{root}/project-alpha/").as_str())
    );
    assert_eq!(typing.option_path(1), None, "no second candidate");

    // Once the text names a directory, they live INSIDE it — so the base moves with them, and a
    // name alone could not say what the field would become.
    let inside = complete_dir(&format!("{root}/project-alpha"));
    assert_eq!(inside.base, d.path().join("project-alpha"));
    assert!(inside.options.is_empty(), "the directory is empty");

    // Joined, never concatenated: a base of `/` must not produce `//usr/`.
    let rooted = complete_dir("/us");
    assert!(
        rooted.option_path(0).is_none_or(|p| !p.starts_with("//")),
        "{rooted:?}"
    );
}
