// Support materialisation returns the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Cursor, Read, Write},
    os::{
        fd::{AsFd, OwnedFd},
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::{Component, Path},
};

use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, RenameFlags, ResolveFlags, fchmod, fstat, fsync,
        mkdirat, openat2, renameat_with, statat, unlinkat,
    },
    io::{Errno, dup},
};
use sha2::{Digest, Sha256};

use super::{EmbeddedHostError, SupportMember, digest_hex};

const DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o700);
const FILE_MODE: Mode = Mode::from_raw_mode(0o600);
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW);
const CHILD_RESOLUTION: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_XDEV);
const ROOT_RESOLUTION: ResolveFlags = ResolveFlags::NO_MAGICLINKS.union(ResolveFlags::NO_SYMLINKS);
const MAXIMUM_DIRECTORY_DEPTH: usize = 64;

pub(super) fn materialise_support_tree(
    root: &Path,
    members: &[SupportMember],
    archive_bytes: &[u8],
) -> Result<(), EmbeddedHostError> {
    let (parent_path, root_name) = split_root(root)?;
    let parent = open_root_directory(parent_path)?;
    require_private_directory(&parent)?;

    if entry_exists(&parent, root_name)? {
        if let Ok(existing) = open_child(&parent, root_name, DIRECTORY_FLAGS, Mode::empty())
            && verify_tree(&existing, members).is_ok()
        {
            return Ok(());
        }
        remove_entry(&parent, root_name, 0)?;
        sync(&parent)?;
    }

    create_directory(&parent, root_name)?;
    let materialisation = (|| {
        let root = open_child(&parent, root_name, DIRECTORY_FLAGS, Mode::empty())?;
        require_private_directory(&root)?;
        write_archive(&root, members, archive_bytes)?;
        verify_tree(&root, members)?;
        sync(&root)?;
        sync(&parent)
    })();

    if let Err(error) = materialisation {
        remove_entry(&parent, root_name, 0)?;
        sync(&parent)?;
        return Err(error);
    }
    Ok(())
}

fn split_root(root: &Path) -> Result<(&Path, &OsStr), EmbeddedHostError> {
    if !root.is_absolute() {
        return Err(EmbeddedHostError::InvalidSupportPath);
    }
    let parent = root.parent().ok_or(EmbeddedHostError::InvalidSupportPath)?;
    let name = root
        .file_name()
        .ok_or(EmbeddedHostError::InvalidSupportPath)?;
    if name.is_empty()
        || name.as_bytes().contains(&0)
        || !matches!(root.components().next_back(), Some(Component::Normal(_)))
    {
        return Err(EmbeddedHostError::InvalidSupportPath);
    }
    Ok((parent, name))
}

fn open_root_directory(path: &Path) -> Result<OwnedFd, EmbeddedHostError> {
    openat2(CWD, path, DIRECTORY_FLAGS, Mode::empty(), ROOT_RESOLUTION).map_err(io_error)
}

fn open_child(
    parent: impl AsFd,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> Result<OwnedFd, EmbeddedHostError> {
    openat2(parent, name, flags, mode, CHILD_RESOLUTION).map_err(io_error)
}

fn create_directory(parent: impl AsFd, name: &OsStr) -> Result<(), EmbeddedHostError> {
    mkdirat(&parent, name, DIRECTORY_MODE).map_err(io_error)?;
    let directory = open_child(parent, name, DIRECTORY_FLAGS, Mode::empty())?;
    fchmod(&directory, DIRECTORY_MODE).map_err(io_error)?;
    require_private_directory(&directory)?;
    sync(&directory)
}

fn entry_exists(parent: impl AsFd, name: &OsStr) -> Result<bool, EmbeddedHostError> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(io_error(error)),
    }
}

fn write_archive(
    root: &OwnedFd,
    members: &[SupportMember],
    archive_bytes: &[u8],
) -> Result<(), EmbeddedHostError> {
    let mut archive = tar::Archive::new(Cursor::new(archive_bytes));
    let mut entries = archive.entries()?;
    for (index, expected) in members.iter().enumerate() {
        let mut entry = entries
            .next()
            .ok_or(EmbeddedHostError::SupportMismatch("bundle is incomplete"))??;
        require_archive_metadata(&entry, expected)?;

        let (directory, file_name) = member_parent(root, &expected.path, true)?;
        let temporary_name = OsString::from(format!(".orna-support-tmp-{index:04}"));
        let output = open_child(
            &directory,
            &temporary_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            FILE_MODE,
        )?;
        let mut output = File::from(output);
        let (length, digest) = stream_digest(&mut entry, Some(&mut output))?;
        fchmod(&output, FILE_MODE).map_err(io_error)?;
        output.sync_all()?;
        if length != expected.length || digest != expected.sha256 {
            return Err(EmbeddedHostError::SupportMismatch(
                "bundle member bytes are not accepted",
            ));
        }
        drop(output);
        renameat_with(
            &directory,
            &temporary_name,
            &directory,
            file_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(io_error)?;
        sync(&directory)?;
    }
    if entries.next().transpose()?.is_some() {
        return Err(EmbeddedHostError::SupportMismatch(
            "bundle has an additional member",
        ));
    }
    Ok(())
}

fn require_archive_metadata<R: Read>(
    entry: &tar::Entry<'_, R>,
    expected: &SupportMember,
) -> Result<(), EmbeddedHostError> {
    let header = entry.header();
    let path = entry
        .path()?
        .to_str()
        .ok_or(EmbeddedHostError::InvalidSupportManifest)?
        .to_owned();
    if path != expected.path
        || !header.entry_type().is_file()
        || header.mode()? != 0o600
        || header.uid()? != 0
        || header.gid()? != 0
        || header.size()? != expected.length
    {
        return Err(EmbeddedHostError::SupportMismatch(
            "bundle metadata is not accepted",
        ));
    }
    Ok(())
}

fn member_parent<'a>(
    root: &OwnedFd,
    member_path: &'a str,
    create: bool,
) -> Result<(OwnedFd, &'a OsStr), EmbeddedHostError> {
    let path = Path::new(member_path);
    let file_name = path
        .file_name()
        .ok_or(EmbeddedHostError::InvalidSupportPath)?;
    let mut current = dup(root).map_err(io_error)?;
    let parent = path.parent().ok_or(EmbeddedHostError::InvalidSupportPath)?;
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(EmbeddedHostError::InvalidSupportPath);
        };
        let created = if create {
            match mkdirat(&current, name, DIRECTORY_MODE) {
                Ok(()) => {
                    sync(&current)?;
                    true
                }
                Err(Errno::EXIST) => false,
                Err(error) => return Err(io_error(error)),
            }
        } else {
            false
        };
        let next = open_child(&current, name, DIRECTORY_FLAGS, Mode::empty())?;
        if created {
            fchmod(&next, DIRECTORY_MODE).map_err(io_error)?;
        }
        require_private_directory(&next)?;
        current = next;
    }
    Ok((current, file_name))
}

fn verify_tree(root: &OwnedFd, members: &[SupportMember]) -> Result<(), EmbeddedHostError> {
    require_private_directory(root)?;
    let expected = members
        .iter()
        .map(|member| member.path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    collect_inventory(root, Path::new(""), &mut actual, 0)?;
    if actual != expected {
        return Err(EmbeddedHostError::SupportMismatch(
            "materialised inventory is not accepted",
        ));
    }

    for member in members {
        let (directory, file_name) = member_parent(root, &member.path, false)?;
        let file = open_child(&directory, file_name, FILE_FLAGS, Mode::empty())?;
        let stat = fstat(&file).map_err(io_error)?;
        let owner = effective_identity();
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_mode & 0o7777 != 0o600
            || (stat.st_uid, stat.st_gid) != owner
            || stat.st_size < 0
            || stat.st_size as u64 != member.length
        {
            return Err(EmbeddedHostError::SupportMismatch(
                "materialised member metadata is not accepted",
            ));
        }
        let mut file = File::from(file);
        let (length, digest) = stream_digest(&mut file, None)?;
        if length != member.length || digest != member.sha256 {
            return Err(EmbeddedHostError::SupportMismatch(
                "materialised member digest is not accepted",
            ));
        }
    }
    Ok(())
}

fn collect_inventory(
    directory: &OwnedFd,
    relative: &Path,
    paths: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), EmbeddedHostError> {
    require_depth(depth)?;
    for name in directory_entries(directory)? {
        let stat = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
        let path = relative.join(&name);
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                let child = open_child(directory, &name, DIRECTORY_FLAGS, Mode::empty())?;
                require_private_directory(&child)?;
                collect_inventory(&child, &path, paths, depth + 1)?;
            }
            FileType::RegularFile => {
                let path = path
                    .to_str()
                    .ok_or(EmbeddedHostError::InvalidSupportPath)?
                    .to_owned();
                paths.insert(path);
            }
            _ => {
                return Err(EmbeddedHostError::SupportMismatch(
                    "materialised tree contains a link or special file",
                ));
            }
        }
    }
    Ok(())
}

fn remove_entry(parent: &OwnedFd, name: &OsStr, depth: usize) -> Result<(), EmbeddedHostError> {
    require_depth(depth)?;
    let stat = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
        let directory = open_child(parent, name, DIRECTORY_FLAGS, Mode::empty())?;
        for child in directory_entries(&directory)? {
            remove_entry(&directory, &child, depth + 1)?;
        }
        sync(&directory)?;
        drop(directory);
        unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(io_error)?;
    } else {
        unlinkat(parent, name, AtFlags::empty()).map_err(io_error)?;
    }
    Ok(())
}

fn directory_entries(directory: &OwnedFd) -> Result<Vec<OsString>, EmbeddedHostError> {
    let mut entries = Vec::new();
    let directory = Dir::read_from(directory).map_err(io_error)?;
    for entry in directory {
        let entry = entry.map_err(io_error)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        entries.push(OsString::from_vec(bytes.to_vec()));
    }
    Ok(entries)
}

fn require_private_directory(directory: impl AsFd) -> Result<(), EmbeddedHostError> {
    let stat = fstat(directory).map_err(io_error)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || stat.st_mode & 0o7777 != 0o700
        || (stat.st_uid, stat.st_gid) != effective_identity()
    {
        return Err(EmbeddedHostError::SupportMismatch(
            "support directory metadata is not accepted",
        ));
    }
    Ok(())
}

fn require_depth(depth: usize) -> Result<(), EmbeddedHostError> {
    if depth <= MAXIMUM_DIRECTORY_DEPTH {
        Ok(())
    } else {
        Err(EmbeddedHostError::SupportMismatch(
            "materialised tree is too deep",
        ))
    }
}

fn sync(file: impl AsFd) -> Result<(), EmbeddedHostError> {
    fsync(file).map_err(io_error)
}

fn stream_digest(
    input: &mut impl Read,
    mut output: Option<&mut File>,
) -> Result<(u64, String), EmbeddedHostError> {
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&buffer[..count])?;
        }
        digest.update(&buffer[..count]);
        length = length
            .checked_add(count as u64)
            .ok_or(EmbeddedHostError::SupportMismatch("member is too large"))?;
    }
    Ok((length, digest_hex(digest.finalize())))
}

fn effective_identity() -> (u32, u32) {
    // SAFETY: these libc identity getters have no preconditions.
    unsafe { (nix::libc::geteuid(), nix::libc::getegid()) }
}

fn io_error(error: Errno) -> EmbeddedHostError {
    EmbeddedHostError::Io(io::Error::from(error))
}
