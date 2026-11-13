use super::*;

pub(crate) const DISCOVER_WORKTREE_HELP: &str =
    "run the command inside a Git worktree or provide a local project path";

/// The local Git worktree `endpoint` names, or the `E2100` refusal whose
/// remedy is `help`.
pub(crate) fn discover_worktree(
    endpoint: &Endpoint,
    help: &'static str,
) -> Result<orna_repository::Repository, Diagnostic> {
    orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
        Diagnostic::target("E2100", "local Git worktree could not be discovered", help)
    })
}

pub(crate) fn run_status(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args([
            "-c",
            "color.status=false",
            "status",
            "--porcelain",
        ]),
        "check that Git can read the local worktree, then retry `status --porcelain`",
    )?;
    io::stdout().write_all(&output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --porcelain`",
        )
    })?;
    Ok(())
}

pub(crate) fn run_status_json(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args([
            "-c",
            "color.status=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ]),
        "check that Git can read the local worktree, then retry `status --format json`",
    )?;
    let branch_output = cli_status::git_in(repository.worktree())
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree branch could not be read",
                "check that Git can read the local worktree, then retry `status --format json`",
            )
        })?;
    let branch = cli_status::branch_name(branch_output.status.success(), &branch_output.stdout);
    let json = cli_status::encode_status_json(branch, &output.stdout).ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "retry `status --format json`",
        )
    })?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    let json = append_unpublished_count_json(json, unpublished_count).ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "retry `status --format json`",
        )
    })?;
    io::stdout().lock().write_all(&json).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --format json`",
        )
    })?;
    Ok(())
}

pub(crate) fn unpublished_mutation_count(
    repository: &orna_repository::Repository,
) -> Result<usize, Diagnostic> {
    if !repository.runtime_paths().state_db().exists() {
        return Ok(0);
    }
    let (identity, initial_digest) = runtime_identity(repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })?;
    runtime
        .block_on(state.publication_metadata_rows())
        .and_then(|rows| {
            if rows.sys_storage != rows.maintenance_job {
                return Err(orna_runtime::RuntimeError::RecoveryInvalid);
            }
            match rows.sys_storage.raw() {
                OvbRaw::Map(fields) => fields
                    .iter()
                    .find_map(|(key, value)| match (key, value) {
                        (OvbRaw::Text(key), OvbRaw::Int(value)) if key == "pending_rows" => {
                            usize::try_from(value.clone()).ok()
                        }
                        _ => None,
                    })
                    .ok_or(orna_runtime::RuntimeError::RecoveryInvalid),
                _ => Err(orna_runtime::RuntimeError::RecoveryInvalid),
            }
        })
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })
}

/// Counts the unpublished tail of a migration source without writing to it.
/// The count is read through a read-only handle to `state.db`: no runtime is
/// opened, no owner lock is taken and no schema is created or migrated, so a
/// source that refuses a copy is left byte-identical.
pub(crate) fn source_unpublished_mutation_count(
    repository: &orna_repository::Repository,
) -> Result<usize, Diagnostic> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `migrate copy` after checking the source runtime state",
            )
        })?;
    runtime
        .block_on(orna_runtime::RuntimeState::read_pending_count(repository))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `migrate copy` after checking the source runtime state",
            )
        })
}

pub(crate) fn runtime_publication_metadata_rows(
    endpoint: &Endpoint,
    std_snapshot: StdSnapshotPin,
) -> Result<(RuntimePublicationMetadataRows, SysHostContext), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "local runtime publication metadata could not be read",
            "run the command inside an initialized local Orna repository",
        )
    })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let sys_host_context = sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Embedded,
        std_snapshot,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let rows = runtime
        .block_on(state.publication_metadata_rows())
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    Ok((rows, sys_host_context))
}

pub(crate) fn write_unpublished_summary<W: io::Write>(
    writer: &mut W,
    count: usize,
) -> io::Result<()> {
    let noun = if count == 1 { "mutation" } else { "mutations" };
    writeln!(writer, "Unpublished CWD changes: {count} runtime {noun}")
}

pub(crate) fn append_unpublished_count_json(mut json: Vec<u8>, count: usize) -> Option<Vec<u8>> {
    let prefix = json.strip_suffix(b"]}\n")?.len();
    json.truncate(prefix);
    json.extend_from_slice(b"],\"unpublished_runtime_mutations\":");
    json.extend_from_slice(count.to_string().as_bytes());
    json.extend_from_slice(b"}\n");
    Some(json)
}

pub(crate) fn osc8_terminal_supports(
    term: Option<&str>,
    terminal_program: Option<&str>,
    terminal_program_version: Option<&str>,
    vte_version: Option<&str>,
    windows_terminal_session: Option<&str>,
    inside_tmux: bool,
) -> bool {
    if inside_tmux {
        return false;
    }

    let known_term = matches!(
        term,
        Some(
            "xterm-kitty"
                | "xterm-ghostty"
                | "foot"
                | "foot-extra"
                | "wezterm"
                | "wezterm-256color"
                | "alacritty"
                | "contour"
        )
    );
    let known_program = matches!(terminal_program, Some("WezTerm" | "ghostty"));
    let iterm_supports_osc8 = terminal_program == Some("iTerm.app")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (3, 1, 0)));
    let alacritty_supports_osc8 = terminal_program == Some("Alacritty")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (0, 13, 0)));
    let vscode_supports_osc8 = terminal_program == Some("vscode")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (1, 72, 0)));
    let vte_supports_osc8 = vte_version
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version >= 5000);
    let windows_terminal_supports_osc8 =
        windows_terminal_session.is_some_and(|session| !session.is_empty());

    known_term
        || known_program
        || iterm_supports_osc8
        || alacritty_supports_osc8
        || vscode_supports_osc8
        || vte_supports_osc8
        || windows_terminal_supports_osc8
}

pub(crate) fn version_at_least(version: &str, minimum: (u32, u32, u32)) -> bool {
    let mut components = version.split('.');
    let Some(major) = components.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let Some(minor) = components.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let patch = match components.next() {
        Some(part) => match part.parse::<u32>() {
            Ok(patch) => patch,
            Err(_) => return false,
        },
        None => 0,
    };
    (major, minor, patch) >= minimum
}

pub(crate) fn terminal_supports_osc8() -> bool {
    let term = std::env::var("TERM").ok();
    let terminal_program = std::env::var("TERM_PROGRAM").ok();
    let terminal_program_version = std::env::var("TERM_PROGRAM_VERSION").ok();
    let vte_version = std::env::var("VTE_VERSION").ok();
    let windows_terminal_session = std::env::var("WT_SESSION").ok();
    let inside_tmux = std::env::var_os("TMUX").is_some();
    osc8_terminal_supports(
        term.as_deref(),
        terminal_program.as_deref(),
        terminal_program_version.as_deref(),
        vte_version.as_deref(),
        windows_terminal_session.as_deref(),
        inside_tmux,
    )
}

#[allow(clippy::items_after_statements)]
pub(crate) fn file_url_for_status_path(
    root: &std::path::Path,
    display_token: &[u8],
) -> Option<String> {
    if !root.is_absolute()
        || display_token.is_empty()
        || display_token.first() == Some(&b'"')
        || display_token.iter().any(u8::is_ascii_control)
    {
        return None;
    }
    let path_text = std::str::from_utf8(display_token).ok()?;
    if path_text.chars().any(char::is_control) {
        return None;
    }

    let relative_path = std::path::Path::new(path_text);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let absolute_path = root.join(relative_path);
    let absolute_path = absolute_path.to_str()?;
    let absolute_path = if cfg!(windows) {
        absolute_path.replace('\\', "/")
    } else {
        absolute_path.to_owned()
    };
    let mut url = String::with_capacity(7 + absolute_path.len().saturating_mul(3));
    if cfg!(windows) {
        url.push_str("file:///");
    } else {
        url.push_str("file://");
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in absolute_path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    Some(url)
}

#[allow(clippy::items_after_statements)]
pub(crate) fn write_status_path<W: io::Write>(
    writer: &mut W,
    color: AnsiColor,
    path: &[u8],
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    let Some(url) = link_root.and_then(|root| file_url_for_status_path(root, path)) else {
        return write_styled(writer, color, path, color_enabled);
    };
    writer.write_all(b"\x1b]8;;")?;
    writer.write_all(url.as_bytes())?;
    writer.write_all(b"\x1b\\")?;
    write_styled(writer, color, path, color_enabled)?;
    writer.write_all(b"\x1b]8;;\x1b\\")
}

pub(crate) fn write_human_status_path<W: io::Write>(
    writer: &mut W,
    color: AnsiColor,
    action: &[u8],
    path: &[u8],
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    if matches!(action, b"renamed" | b"copied") {
        let separator = b" -> ";
        let Some(separator_start) = path
            .windows(separator.len())
            .position(|window| window == separator)
        else {
            return write_styled(writer, color, path, color_enabled);
        };
        if path[separator_start + separator.len()..]
            .windows(separator.len())
            .any(|window| window == separator)
        {
            return write_styled(writer, color, path, color_enabled);
        }
        write_status_path(
            writer,
            color,
            &path[..separator_start],
            color_enabled,
            link_root,
        )?;
        writer.write_all(separator)?;
        return write_status_path(
            writer,
            color,
            &path[separator_start + separator.len()..],
            color_enabled,
            link_root,
        );
    }
    write_status_path(writer, color, path, color_enabled, link_root)
}

pub(crate) fn write_human_status<W: io::Write>(
    output: &[u8],
    writer: &mut W,
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        let has_newline = line.last() == Some(&b'\n');
        let content = &line[..line.len() - usize::from(has_newline)];
        if let Some(branch) = content.strip_prefix(b"On branch ") {
            writer.write_all(b"On branch ")?;
            write_styled(writer, AnsiColor::Cyan, branch, color_enabled)?;
        } else if content.starts_with(b"Changes to be committed:")
            || content.starts_with(b"Changes not staged for commit:")
            || content.starts_with(b"Untracked files:")
        {
            write_styled(writer, AnsiColor::Yellow, content, color_enabled)?;
        } else if content.starts_with(b"nothing to commit") {
            write_styled(writer, AnsiColor::Green, content, color_enabled)?;
        } else if content.starts_with(b"Your branch ") || content.starts_with(b"  (use ") {
            write_styled(writer, AnsiColor::Dim, content, color_enabled)?;
        } else if content.first() == Some(&b'\t')
            && let Some(colon) = content.iter().position(|byte| *byte == b':')
        {
            let action = &content[1..colon];
            let rest = &content[colon + 1..];
            let path_start = rest
                .iter()
                .position(|byte| !matches!(*byte, b' ' | b'\t'))
                .unwrap_or(rest.len());
            let color = if action.starts_with(b"both ")
                || action.starts_with(b"unmerged")
                || action.starts_with(b"added by ")
                || action.starts_with(b"deleted by ")
            {
                AnsiColor::Red
            } else {
                AnsiColor::Yellow
            };
            writer.write_all(b"\t")?;
            write_styled(writer, color, action, color_enabled)?;
            writer.write_all(b":")?;
            writer.write_all(&rest[..path_start])?;
            write_human_status_path(
                writer,
                AnsiColor::Cyan,
                action,
                &rest[path_start..],
                color_enabled,
                link_root,
            )?;
        } else if content.first() == Some(&b'\t') && content.len() > 1 {
            let rest = &content[1..];
            let path_start = rest
                .iter()
                .position(|byte| !matches!(*byte, b' ' | b'\t'))
                .unwrap_or(rest.len());
            writer.write_all(b"\t")?;
            writer.write_all(&rest[..path_start])?;
            let path = &rest[path_start..];
            if path.first() == Some(&b'(') {
                write_styled(writer, AnsiColor::Cyan, path, color_enabled)?;
            } else {
                write_human_status_path(
                    writer,
                    AnsiColor::Cyan,
                    b"",
                    path,
                    color_enabled,
                    link_root,
                )?;
            }
        } else {
            writer.write_all(content)?;
        }
        if has_newline {
            writer.write_all(b"\n")?;
        }
    }
    Ok(())
}

pub(crate) fn write_short_status<W: io::Write>(
    output: &[u8],
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        let has_newline = line.last() == Some(&b'\n');
        let content = &line[..line.len() - usize::from(has_newline)];
        if content.len() >= 3 && content[2] == b' ' {
            let code = &content[..2];
            let color = if code[0] == b'U'
                || code[1] == b'U'
                || matches!(
                    (code[0], code[1]),
                    (b'D' | b'U', b'D') | (b'A' | b'U', b'A') | (b'A' | b'D', b'U')
                ) {
                AnsiColor::Red
            } else {
                AnsiColor::Yellow
            };
            write_styled(writer, color, code, color_enabled)?;
            writer.write_all(b" ")?;
            write_styled(writer, AnsiColor::Cyan, &content[3..], color_enabled)?;
        } else {
            writer.write_all(content)?;
        }
        if has_newline {
            writer.write_all(b"\n")?;
        }
    }
    Ok(())
}

pub(crate) fn run_status_human(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args(["-c", "color.status=false", "status"]),
        "check that Git can read the local worktree, then retry `status`",
    )?;
    let stdout = io::stdout();
    let link_root = if stdout.is_terminal() && terminal_supports_osc8() {
        repository.worktree().canonicalize().ok()
    } else {
        None
    };
    let mut stdout = stdout.lock();
    write_human_status(
        &output.stdout,
        &mut stdout,
        color_enabled,
        link_root.as_deref(),
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status`",
        )
    })?;
    write_unpublished_summary(&mut stdout, unpublished_count).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status`",
        )
    })?;
    Ok(())
}

pub(crate) fn run_status_short(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    const RETRY: &str = "check that Git can read the local worktree, then retry `status --short`";
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let git_dir_output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args(["rev-parse", "--git-dir"]),
        RETRY,
    )?;
    let git_dir = String::from_utf8(git_dir_output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            RETRY,
        )
    })?;
    let git_dir = std::path::Path::new(git_dir.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_owned()
    } else {
        repository.worktree().join(git_dir)
    };
    let output = cli_status::git_output(
        cli_status::scrubbed_git()
            .arg("--git-dir")
            .arg(&git_dir)
            .arg("--work-tree")
            .arg(repository.worktree())
            .args(["-c", "color.status=false", "status", "--short"]),
        RETRY,
    )?;
    let mut stdout = io::stdout().lock();
    write_short_status(&output.stdout, &mut stdout, color_enabled).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --short`",
        )
    })?;
    Ok(())
}
