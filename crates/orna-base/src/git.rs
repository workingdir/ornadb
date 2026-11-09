//! Git metadata path refusal rules.
//!
//! The repository layer and the host layer below it must agree on which path
//! components name Git's administration entry, so the predicate lives in the
//! primitives crate. It moves with the Git graph when `orna-git` is extracted.

/// Reports whether one path component names Git's administration entry: the
/// `.git` directory or a linked worktree's `.git` file. It matches the names
/// `git fsck` reports as `hasDotgit`, so a repository stays safe to check out
/// on Windows and macOS. Git's `is_ntfs_dotgit` rule applies to the name and
/// to each part after a `\`: `.git` or the 8.3 alias `git~1` in any ASCII
/// case, then only dots or spaces up to the end or a `:` stream suffix. Git's
/// `is_hfs_dotgit` rule applies to the whole name: exactly `.git` in any ASCII
/// case once the HFS+ ignorable code points (U+200C..U+200F, U+202A..U+202E,
/// U+206A..U+206F, U+FEFF) are removed.
///
/// Ordinary names such as `.gitignore`, `.github`, `.gitmodules`, `foo.git`,
/// `git~2`, and `git-notes.md` do not match. Case folding is ASCII-only, as in
/// Git, so `.gıt` (U+0131) and `.GİT` (U+0130) stay ordinary names: Git 2.53
/// accepts both. Whether an NTFS `$UpCase` table folds either into ASCII is
/// unverified on a Windows volume. `.git:x`, `.git.`, and `a\.git` are
/// ordinary names on Linux but are refused here, as Git's default
/// `core.protectNTFS` refuses them. A name that is not UTF-8 is compared as
/// bytes and never meets the HFS+ rule, so `.git\xFF` stays an ordinary name
/// although `git fsck` flags it. This is a refusal predicate for Git metadata
/// names only, not a complete Windows or macOS path validator.
#[must_use]
pub fn is_git_metadata_component(name: &[u8]) -> bool {
    name.split(|byte| *byte == b'\\').any(is_ntfs_git_name) || is_hfs_git_name(name)
}

/// Git's `is_ntfs_dotgit` for one `\`-free part of a name.
fn is_ntfs_git_name(part: &[u8]) -> bool {
    let stem = part.split(|byte| *byte == b':').next().unwrap_or_default();
    let mut end = stem.len();
    while end > 0 && matches!(stem[end - 1], b'.' | b' ') {
        end -= 1;
    }
    let base = &stem[..end];
    base.eq_ignore_ascii_case(b".git") || base.eq_ignore_ascii_case(b"git~1")
}

/// Git's `is_hfs_dotgit`: exactly `.git` once HFS+ ignorables are removed.
fn is_hfs_git_name(name: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(name) else {
        return false;
    };
    let mut visible = text
        .chars()
        .filter(|character| !is_hfs_ignorable(*character))
        .map(|character| character.to_ascii_lowercase());
    ".git"
        .chars()
        .all(|expected| visible.next() == Some(expected))
        && visible.next().is_none()
}

/// Code points that macOS HFS+ ignores when comparing names.
fn is_hfs_ignorable(character: char) -> bool {
    matches!(
        character,
        '\u{200C}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{206A}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both lists follow what `git fsck --strict` (Git 2.53) reports as
    // `hasDotgit` for a tree entry with that name.
    #[test]
    fn refuses_git_administration_names_in_every_spelling() {
        for refused in [
            ".git",
            ".GIT",
            ".Git",
            ".gIt",
            ".git.",
            ".git..",
            ".git ",
            ".GIT. ",
            "git~1",
            "GIT~1",
            "git~1.",
            ".git:",
            ".git:x",
            ".git::$INDEX_ALLOCATION",
            ".git\\config",
            "a\\.git",
            "a\\git~1.",
            "x\\.GIT:y",
            "a:b\\.git",
            ".g\u{200C}it",
            ".\u{FEFF}git",
            ".gi\u{202E}t",
            ".git\u{206F}",
        ] {
            assert!(
                is_git_metadata_component(refused.as_bytes()),
                "{refused:?} must be refused as git metadata"
            );
        }
    }

    #[test]
    fn accepts_ordinary_names_that_only_resemble_git_metadata() {
        for accepted in [
            ".gitignore",
            ".github",
            "foo.git",
            ".gitmodules",
            "git-notes.md",
            "git",
            "git~2",
            "digit",
            "..",
            ".",
            "",
            "gitx.",
            "a.git.b",
            "foo.git\\x",
            "a\\.github",
            ".gıt",
            ".GİT",
            "gıt~1",
            ".g\u{200B}it",
            ".g\u{200C}it.",
            "git~\u{200C}1",
            "a\\.g\u{200C}it",
        ] {
            assert!(
                !is_git_metadata_component(accepted.as_bytes()),
                "{accepted:?} must stay an ordinary name"
            );
        }
    }

    #[test]
    fn non_utf8_names_compare_bytes_only() {
        assert!(is_git_metadata_component(b".GIT"));
        assert!(is_git_metadata_component(b"\xFF\\.git"));
        assert!(!is_git_metadata_component(b".git\xFF"));
    }
}
