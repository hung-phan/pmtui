//! Directory completion for the create form's Directory row.
//!
//! Answers two different questions about one typed path, because a human asks both at once:
//!
//! - **What would you finish this with?** — [`DirCompletion::tail`], the text to APPEND, never the
//!   whole path, so the caller can draw it as a ghost after what was typed and accept it with one
//!   `insert_str`.
//! - **What is there?** — [`DirCompletion::options`], every directory the typed text could still
//!   become, for the list under the row. When the text already names a directory this lists what is
//!   INSIDE it, not that directory's own name back at you: the question is where you can go next.
//!
//! DIRECTORIES ONLY. The field names a project root, so a file is never a candidate: offering one
//! would complete to something the form must then reject.
//!
//! The rules are the ones a shell teaches, because this field is used by people who live in one:
//! complete to the longest unambiguous prefix; append `/` on a unique match so the next keystroke
//! descends; never surface a dotfile unless the fragment already starts with a dot.
//!
//! `$HOME` enters through ONE seam, [`complete_dir_in`], so the tilde rules are tested against a
//! scratch home instead of the machine's — this crate's tests never mutate the environment.

use std::path::{Path, PathBuf};

/// How many entries of one directory are examined. A project root lives among tens of siblings,
/// not thousands, and this runs on EVERY keystroke in the field — so a pathological directory
/// (`/nix/store`, a Maildir) must cost a bounded read rather than a stalled keystroke. Past the
/// bound completion simply stops offering, rather than offering something misleading.
const MAX_ENTRIES: usize = 2_048;

/// How many candidates are KEPT for the list. The list is a hint, not a file manager: past this
/// many the answer is "keep typing", and holding every name of a huge directory on every keystroke
/// would be allocation churn nobody reads. The ghost is unaffected — it is computed from the
/// fragment, not from this list.
const MAX_OPTIONS: usize = 64;

/// What the typed text could become: the one completion to append, and every candidate to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirCompletion {
    /// The text to append, or `None` when there is nothing worth a key.
    ///
    /// `None` rather than an empty string for every "no": an empty tail and "no suggestion" render
    /// identically but mean different things to the key handler, which must fall through to the
    /// next field when there is nothing to accept.
    pub(crate) tail: Option<String>,
    /// The candidate directory names, sorted — `readdir` order is arbitrary, and a list in
    /// arbitrary order is harder to read than no list.
    pub(crate) options: Vec<String>,
    /// The directory the candidates live in. Carried because the names alone cannot say what the
    /// field would BECOME: `base.join(option)` is the whole path, which is what the list shows and
    /// what picking one puts in the field. It is not always the parent of the typed fragment — once
    /// the text names a directory, the candidates are inside it.
    pub(crate) base: PathBuf,
}

impl DirCompletion {
    /// The full path picking `options[i]` would produce, with the trailing separator that says it
    /// is a directory. `join` rather than string concatenation, so a `base` of `/` does not become
    /// `//name`.
    pub(crate) fn option_path(&self, i: usize) -> Option<String> {
        let name = self.options.get(i)?;
        Some(format!("{}/", self.base.join(name).display()))
    }
}

/// Expand a leading `~` against `home`, which is how people actually type a home-relative path.
///
/// Only a leading `~/` or a bare `~`: `~other` is another user's home, which this does not resolve
/// and must not silently treat as the current user's.
fn expand_tilde(input: &str, home: &Path) -> Option<PathBuf> {
    match input {
        "~" => Some(home.to_path_buf()),
        rest => rest.strip_prefix("~/").map(|tail| home.join(tail)),
    }
}

/// Split what was typed into the directory to read and the partial name being matched.
///
/// `"/a/b/pro"` -> read `/a/b`, match `pro`. A trailing slash leaves the fragment empty, which
/// matches every child.
fn split(text: &str) -> Option<(PathBuf, String)> {
    // Rsplit on the separator rather than using `Path::parent`, because `parent` discards the
    // distinction this needs: `"/a/b"` could be a complete directory or a partial name inside
    // `/a`, and only the raw text says which one the human is still typing.
    let (dir, frag) = text.rsplit_once('/')?;
    let dir = if dir.is_empty() { "/" } else { dir };
    Some((PathBuf::from(dir), frag.to_string()))
}

/// The sorted directory names in `dir` that start with `frag`.
fn matching_dirs(dir: &Path, frag: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.take(MAX_ENTRIES) {
        let Ok(entry) = entry else { continue };
        // `file_type` first: it answers from the directory entry on every filesystem that carries
        // the type inline, where a stat would hit each child. It reports the LINK, though, so a
        // symlink falls back to `fs::metadata`, which FOLLOWS it — `DirEntry::metadata` does not,
        // so it would reject a symlinked project root (plenty of people keep one).
        let is_dir = match entry.file_type() {
            Ok(t) if t.is_dir() => true,
            Ok(t) if t.is_symlink() => std::fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false),
            _ => false,
        };
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(frag) {
            continue;
        }
        // A dotfile is noise until the human shows they want one by typing the dot.
        if name.starts_with('.') && !frag.starts_with('.') {
            continue;
        }
        names.push(name);
    }
    names.sort();
    names
}

/// The longest prefix `first` shares with every name in `rest`. Takes the first candidate
/// separately because there is always one — a no-candidates case is the caller's, and spelling it
/// here would be an unreachable branch.
fn common_prefix(first: &str, rest: &[String]) -> String {
    let mut len = first.chars().count();
    for name in rest {
        len = len.min(
            first
                .chars()
                .zip(name.chars())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    first.chars().take(len).collect()
}

/// The text to append to complete `frag` from `names`, or `None` when there is nothing to add.
///
/// A tail of just `/` is NOT offered. That is the case of a path already naming a real directory —
/// which every freshly opened form has, since it opens on the working directory — and offering it
/// would spend the `Tab` that was about to leave the row. Type the `/` and the children become
/// real completions, exactly as in a shell.
fn unambiguous_tail(frag: &str, names: &[String]) -> Option<String> {
    let (first, rest) = names.split_first()?;
    let prefix = common_prefix(first, rest);
    let mut tail: String = prefix.chars().skip(frag.chars().count()).collect();
    // A unique match is a directory the human can descend into, so hand them the separator too and
    // save a keystroke. Ambiguous matches get none — their shared prefix is not a directory yet.
    if rest.is_empty() {
        tail.push('/');
    }
    if tail.is_empty() || tail == "/" {
        None
    } else {
        Some(tail)
    }
}

/// What `input` could become. Empty when there is nothing to say about it.
pub(crate) fn complete_dir(input: &str) -> DirCompletion {
    // An absent `HOME` becomes an empty prefix rather than a special case: `~/x` then names `/x`,
    // which simply does not exist, so completion stops offering instead of guessing a home.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    complete_dir_in(input, &home)
}

/// [`complete_dir`] with `$HOME` injected — the seam the tests drive.
pub(crate) fn complete_dir_in(input: &str, home: &Path) -> DirCompletion {
    if input.is_empty() {
        return DirCompletion::default();
    }
    let expanded = expand_tilde(input, home).unwrap_or_else(|| PathBuf::from(input));
    let text = expanded.to_string_lossy().into_owned();
    // A path with no separator at all cannot be split into a directory and a fragment. Returning
    // early also keeps a bare word from being resolved against the PROCESS working directory,
    // which is not a place this field ever meant.
    let Some((dir, frag)) = split(&text) else {
        return DirCompletion::default();
    };
    let siblings = matching_dirs(&dir, &frag);
    let tail = unambiguous_tail(&frag, &siblings);
    // The list answers "where can I go from here". So once the text names a directory, list what is
    // inside it; until then, the siblings it could still turn into.
    //
    // A bare `.` is the exception, because it is BOTH: a real directory entry naming this one, and
    // the first character of a hidden name. The human typing it means the second — they just asked
    // for the dotfiles — so it stays on the sibling path.
    let descend = expanded.is_dir() && frag != ".";
    let mut options = if descend {
        matching_dirs(&expanded, "")
    } else {
        siblings
    };
    options.truncate(MAX_OPTIONS);
    let base = if descend { expanded } else { dir };
    DirCompletion {
        tail,
        options,
        base,
    }
}
