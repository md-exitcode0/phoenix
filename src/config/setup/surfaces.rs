//! Browser + desktop-bridge setup steps of the onboarding wizard.

use super::*;

pub(super) fn ask_login_source(
    theme: &ColorfulTheme,
    current: Option<&str>,
) -> Result<Option<String>> {
    // (label, config value). Firefox-family read plaintext; Chromium-family are
    // decrypted (Brave/Edge need the login keyring unlocked / `secret-tool`).
    let options: [(&str, &str); 9] = [
        ("None — use the browser's own profile", ""),
        ("Zen", "zen"),
        ("Firefox", "firefox"),
        ("LibreWolf", "librewolf"),
        ("Floorp", "floorp"),
        ("Waterfox", "waterfox"),
        ("Chrome", "chrome"),
        ("Brave", "brave"),
        ("Chromium", "chromium"),
    ];
    let default = options
        .iter()
        .position(|(_, v)| Some(*v) == current.map(str::to_lowercase).as_deref())
        .filter(|i| *i != 0)
        .unwrap_or(0);
    let labels: Vec<&str> = options.iter().map(|(l, _)| *l).collect();
    let picked = Select::with_theme(theme)
        .with_prompt("  Port logins from another browser? (copies its logged-in sessions into Chrome, fresh each run)")
        .items(&labels)
        .default(default)
        .interact()
        .context("Login-source selection cancelled")?;
    Ok(match picked {
        0 => None,
        other => Some(options[other].1.to_string()),
    })
}

pub(super) fn run_browser_setup(theme: &ColorfulTheme) -> Result<BrowserSetup> {
    println!();
    println!("  {} Browser agent", style("◆").color256(208).bold());
    // An existing browser config (possibly with a custom binary like
    // CloakBrowser) is offered as-is first — re-running onboard must never
    // silently discard it.
    if let Some(existing) = crate::config::PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.browser)
    {
        let mut summary = existing.source.clone().unwrap_or_else(|| "phoenix".into());
        if let Some(port) = existing.attach_port {
            summary.push_str(&format!(" (attach port {port})"));
        }
        if let Some(binary) = &existing.binary {
            summary.push_str(&format!(" · custom binary {binary}"));
        }
        if Confirm::with_theme(theme)
            .with_prompt(format!("  Keep current browser setup? [{summary}]"))
            .default(true)
            .interact()
            .context("Browser keep-confirmation cancelled")?
        {
            return Ok((
                existing.source,
                existing.attach_port,
                existing.binary,
                existing.extra_args,
                existing.login_source,
                existing.suspend_after_seconds,
            ));
        }
    }
    let current_login = crate::config::PhoenixConfig::load()
        .ok()
        .and_then(|c| c.profile.browser)
        .and_then(|b| b.login_source);
    println!(
        "  {}",
        style("  Phoenix drives a real Chromium. Connecting your own Chrome gives agents your sessions and logins (recommended).").dim()
    );
    // CloakBrowser is offered only when its binary is actually present, so the
    // option never dead-ends on a missing download.
    let cloak = detect_cloakbrowser();
    let mut choices = vec![
        "My Chrome — attach to my own browser (sessions + logins; needs Chrome started with --remote-debugging-port)",
        "Phoenix-managed Chromium profile (separate, clean)",
    ];
    if cloak.is_some() {
        choices.push(
            "Stealth CloakBrowser — anti-bot Chromium with your logins (best for X / social / scraping)",
        );
    }
    choices.push("Skip for now");
    let cloak_index = cloak.as_ref().map(|_| 2);
    let skip_index = choices.len() - 1;
    let picked = Select::with_theme(theme)
        .with_prompt("  Browser connection")
        .items(&choices)
        .default(0)
        .interact()
        .context("Browser selection cancelled")?;
    if Some(picked) == cloak_index {
        let path = cloak.expect("cloak index only set when present");
        println!(
            "  {} Stealth CloakBrowser at {} — launches with a copy of your Chrome logins.",
            style("•").green(),
            style(path.display()).dim()
        );
        // source="chrome" copies the Chrome profile (logins); the binary makes
        // the launch use CloakBrowser instead of stock Chrome.
        let login_source = ask_login_source(theme, current_login.as_deref())?;
        return Ok((
            Some("chrome".to_string()),
            Some(9222),
            Some(path.to_string_lossy().to_string()),
            None,
            login_source,
            None,
        ));
    }
    if picked == skip_index {
        return Ok((None, None, None, None, None, None));
    }
    match picked {
        0 => {
            let port: String = Input::with_theme(theme)
                .with_prompt("  Chrome remote-debugging port")
                .default("9222".to_string())
                .interact_text()
                .context("Port entry cancelled")?;
            let port: u16 = port
                .trim()
                .parse()
                .context("port must be a number, e.g. 9222")?;
            println!(
                "  {} Launch Chrome once with: {}",
                style("•").yellow(),
                style(format!(
                    "google-chrome --remote-debugging-port={port} (add it to your launcher so it's always on)"
                ))
                .dim()
            );
            let login_source = ask_login_source(theme, current_login.as_deref())?;
            Ok((
                Some("chrome".to_string()),
                Some(port),
                None,
                None,
                login_source,
                None,
            ))
        }
        _ => {
            let login_source = ask_login_source(theme, current_login.as_deref())?;
            Ok((
                Some("phoenix".to_string()),
                None,
                None,
                None,
                login_source,
                None,
            ))
        }
    }
}

/// The CloakBrowser binary, if it has been downloaded to the standard path.
pub(super) fn detect_cloakbrowser() -> Option<std::path::PathBuf> {
    let path = crate::config::phoenix_home().join("browser/cloakbrowser/chrome");
    path.is_file().then_some(path)
}

/// Desktop bridge for the computer_use agent: the GNOME extension that draws
/// Phoenix's own animated cursor and injects input. Staged here so onboard is
/// genuinely the FULL setup; enabling still needs one logout (Wayland reloads
/// extensions at login).
pub(super) fn run_desktop_bridge_setup(theme: &ColorfulTheme) -> Result<()> {
    println!();
    println!(
        "  {} Desktop agent (computer_use)",
        style("◆").color256(208).bold()
    );
    let on_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    if !on_wayland {
        println!(
            "  {}",
            style("  X11 session detected — install `xdotool` (sudo apt install xdotool) and the desktop agent is ready. No extension needed.").dim()
        );
        return Ok(());
    }
    let Some(base) = BaseDirs::new() else {
        return Ok(());
    };
    let source = exe_relative_extension_dir();
    let Some(source) = source else {
        println!(
            "  {}",
            style("  Extension source not found next to the binary — run install.sh from the repo to stage it.").dim()
        );
        return Ok(());
    };
    if !Confirm::with_theme(theme)
        .with_prompt(
            "Install the Phoenix cursor extension? (the agent's own animated cursor on GNOME)",
        )
        .default(true)
        .interact()
        .context("Desktop bridge confirmation cancelled")?
    {
        return Ok(());
    }
    let dest = base
        .home_dir()
        .join(".local/share/gnome-shell/extensions/phoenix-cursor@phoenix.dev");
    copy_dir_recursive(&source, &dest)?;
    println!(
        "  {} Extension staged. ONE-TIME: log out and back in, then run:",
        style("✔").green()
    );
    println!(
        "      {}",
        style("gnome-extensions enable phoenix-cursor@phoenix.dev").color256(208)
    );
    Ok(())
}

/// The extension ships next to the installed binary OR in the repo checkout.
pub(super) fn exe_relative_extension_dir() -> Option<PathBuf> {
    const REL: &str = "desktop/gnome-extension/phoenix-cursor@phoenix.dev";
    // Repo layout (cargo run / install.sh from checkout).
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().take(5) {
            let candidate = ancestor.join(REL);
            if candidate.join("metadata.json").exists() {
                return Some(candidate);
            }
        }
    }
    let cwd_candidate = std::env::current_dir().ok()?.join(REL);
    cwd_candidate
        .join("metadata.json")
        .exists()
        .then_some(cwd_candidate)
}

const EXTENSION_MAX_DEPTH: usize = 16;
const EXTENSION_MAX_NODES: usize = 4_096;
const EXTENSION_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const EXTENSION_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const EXTENSION_STAGE_PREFIX: &str = ".phoenix-extension-stage-";

#[derive(Default)]
struct ExtensionCopyBudget {
    nodes: usize,
    bytes: u64,
}

impl ExtensionCopyBudget {
    fn visit(&mut self, path: &std::path::Path) -> Result<()> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .context("extension node counter overflow")?;
        if self.nodes > EXTENSION_MAX_NODES {
            anyhow::bail!(
                "extension tree exceeded {EXTENSION_MAX_NODES} filesystem nodes at {}",
                path.display()
            );
        }
        Ok(())
    }

    fn add_file(&mut self, path: &std::path::Path, bytes: u64) -> Result<()> {
        if bytes > EXTENSION_MAX_FILE_BYTES {
            anyhow::bail!(
                "extension file {} is too large ({bytes} bytes; max {EXTENSION_MAX_FILE_BYTES})",
                path.display()
            );
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("extension byte counter overflow")?;
        if self.bytes > EXTENSION_MAX_TOTAL_BYTES {
            anyhow::bail!(
                "extension tree exceeded {EXTENSION_MAX_TOTAL_BYTES} total bytes at {}",
                path.display()
            );
        }
        Ok(())
    }
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

fn open_extension_directory(path: &std::path::Path, path_is_stable: bool) -> Result<std::fs::File> {
    if !path_is_stable {
        crate::config::private_io::reject_symlink_components(path)?;
    }
    let expected = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect extension directory {}", path.display()))?;
    if expected.file_type().is_symlink() || !expected.is_dir() {
        anyhow::bail!("extension path is not a real directory: {}", path.display());
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY,
        );
    }
    let directory = options
        .open(path)
        .with_context(|| format!("failed to open extension directory {}", path.display()))?;
    let opened = directory
        .metadata()
        .with_context(|| format!("failed to inspect open directory {}", path.display()))?;
    if !opened.is_dir() || !same_file_identity(&expected, &opened) {
        anyhow::bail!(
            "extension directory changed while opening it: {}",
            path.display()
        );
    }
    Ok(directory)
}

/// Enumerate a pinned directory inode on Linux. Renaming an ancestor while a
/// copy is in progress cannot redirect reads or writes through this path.
#[cfg(target_os = "linux")]
fn stable_directory_path(_path: &std::path::Path, directory: &std::fs::File) -> (PathBuf, bool) {
    use std::os::fd::AsRawFd;
    (
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())),
        true,
    )
}

#[cfg(not(target_os = "linux"))]
fn stable_directory_path(path: &std::path::Path, _directory: &std::fs::File) -> (PathBuf, bool) {
    (path.to_path_buf(), false)
}

fn checked_entry_name(name: &std::ffi::OsStr) -> Result<&std::ffi::OsStr> {
    let mut components = std::path::Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(component)), None) if !component.is_empty() => {
            Ok(component)
        }
        _ => anyhow::bail!("unsafe extension entry name: {:?}", name),
    }
}

fn create_private_directory_new(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    };
    #[cfg(not(unix))]
    let result = std::fs::create_dir(path);
    result.with_context(|| format!("failed to create private directory {}", path.display()))?;

    let validation = (|| -> Result<()> {
        let directory = open_extension_directory(path, true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = directory
                .metadata()
                .with_context(|| format!("failed to inspect new directory {}", path.display()))?;
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                anyhow::bail!(
                    "new extension directory is not private and owner-controlled: {}",
                    path.display()
                );
            }
        }
        Ok(())
    })();
    if let Err(error) = validation {
        // This inode was just created with create_dir (no-clobber), so it is
        // safe to remove if post-create validation fails.
        let _ = std::fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

fn copy_extension_file(
    source: &std::path::Path,
    expected: &std::fs::Metadata,
    dest: &std::path::Path,
    budget: &mut ExtensionCopyBudget,
) -> Result<()> {
    use std::io::{Read, Write};

    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = source_options
        .open(source)
        .with_context(|| format!("failed to open extension file {}", source.display()))?;
    let opened = input
        .metadata()
        .with_context(|| format!("failed to inspect open file {}", source.display()))?;
    if !opened.is_file() || !same_file_identity(expected, &opened) {
        anyhow::bail!(
            "extension file changed while opening it: {}",
            source.display()
        );
    }
    if opened.len() > EXTENSION_MAX_FILE_BYTES {
        anyhow::bail!(
            "extension file {} is too large ({} bytes; max {EXTENSION_MAX_FILE_BYTES})",
            source.display(),
            opened.len()
        );
    }

    let mut bytes = Vec::with_capacity(opened.len() as usize);
    std::io::Read::by_ref(&mut input)
        .take(EXTENSION_MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read extension file {}", source.display()))?;
    let final_metadata = input
        .metadata()
        .with_context(|| format!("failed to re-inspect extension file {}", source.display()))?;
    if bytes.len() as u64 > EXTENSION_MAX_FILE_BYTES
        || bytes.len() as u64 != opened.len()
        || !same_file_identity(&opened, &final_metadata)
        || final_metadata.len() != opened.len()
    {
        anyhow::bail!(
            "extension file changed while reading it: {}",
            source.display()
        );
    }
    budget.add_file(source, bytes.len() as u64)?;

    let mut dest_options = std::fs::OpenOptions::new();
    dest_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        dest_options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut output = dest_options
        .open(dest)
        .with_context(|| format!("failed to create extension file {}", dest.display()))?;
    let output_metadata = output
        .metadata()
        .with_context(|| format!("failed to inspect extension file {}", dest.display()))?;
    if !output_metadata.is_file() {
        anyhow::bail!(
            "extension destination is not a regular file: {}",
            dest.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if output_metadata.uid() != unsafe { libc::geteuid() } || output_metadata.nlink() != 1 {
            anyhow::bail!("unsafe extension destination file: {}", dest.display());
        }
        output
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure extension file {}", dest.display()))?;
    }
    output
        .write_all(&bytes)
        .with_context(|| format!("failed to write extension file {}", dest.display()))?;
    output
        .sync_all()
        .with_context(|| format!("failed to sync extension file {}", dest.display()))
}

fn copy_extension_tree(
    source: &std::path::Path,
    dest: &std::path::Path,
    budget: &mut ExtensionCopyBudget,
    depth: usize,
    source_is_stable: bool,
) -> Result<()> {
    if depth > EXTENSION_MAX_DEPTH {
        anyhow::bail!(
            "extension tree exceeded depth {EXTENSION_MAX_DEPTH} at {}",
            source.display()
        );
    }
    let source_directory = open_extension_directory(source, source_is_stable)?;
    let dest_directory = open_extension_directory(dest, true)?;
    let (read_path, children_are_stable) = stable_directory_path(source, &source_directory);
    let (write_path, _) = stable_directory_path(dest, &dest_directory);

    for entry in std::fs::read_dir(&read_path).with_context(|| {
        format!(
            "failed to enumerate extension directory {}",
            source.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to enumerate extension directory {}",
                source.display()
            )
        })?;
        let name = entry.file_name();
        let name = checked_entry_name(&name)?;
        let source_child = entry.path();
        let dest_child = write_path.join(name);
        budget.visit(&source_child)?;
        let metadata = std::fs::symlink_metadata(&source_child).with_context(|| {
            format!(
                "failed to inspect extension entry {}",
                source_child.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "extension source contains a symlink: {}",
                source_child.display()
            );
        }
        if metadata.is_dir() {
            create_private_directory_new(&dest_child)?;
            copy_extension_tree(
                &source_child,
                &dest_child,
                budget,
                depth + 1,
                children_are_stable,
            )?;
        } else if metadata.is_file() {
            copy_extension_file(&source_child, &metadata, &dest_child, budget)?;
        } else {
            anyhow::bail!(
                "extension source contains a special file: {}",
                source_child.display()
            );
        }
    }
    dest_directory
        .sync_all()
        .with_context(|| format!("failed to sync staged directory {}", dest.display()))
}

fn validate_extension_tree(
    root: &std::path::Path,
    budget: &mut ExtensionCopyBudget,
    depth: usize,
    root_is_stable: bool,
) -> Result<()> {
    if depth > EXTENSION_MAX_DEPTH {
        anyhow::bail!(
            "existing extension tree exceeded depth {EXTENSION_MAX_DEPTH} at {}",
            root.display()
        );
    }
    let directory = open_extension_directory(root, root_is_stable)?;
    let (read_path, children_are_stable) = stable_directory_path(root, &directory);
    for entry in std::fs::read_dir(&read_path)
        .with_context(|| format!("failed to enumerate existing extension {}", root.display()))?
    {
        let entry = entry.with_context(|| {
            format!("failed to enumerate existing extension {}", root.display())
        })?;
        let name = entry.file_name();
        checked_entry_name(&name)?;
        let child = entry.path();
        budget.visit(&child)?;
        let expected = std::fs::symlink_metadata(&child).with_context(|| {
            format!(
                "failed to inspect existing extension entry {}",
                child.display()
            )
        })?;
        if expected.file_type().is_symlink() {
            anyhow::bail!("existing extension contains a symlink: {}", child.display());
        }
        if expected.is_dir() {
            validate_extension_tree(&child, budget, depth + 1, children_are_stable)?;
        } else if expected.is_file() {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let file = options.open(&child).with_context(|| {
                format!("failed to open existing extension file {}", child.display())
            })?;
            let opened = file.metadata().with_context(|| {
                format!(
                    "failed to inspect existing extension file {}",
                    child.display()
                )
            })?;
            if !opened.is_file() || !same_file_identity(&expected, &opened) {
                anyhow::bail!(
                    "existing extension changed while opening: {}",
                    child.display()
                );
            }
            budget.add_file(&child, opened.len())?;
        } else {
            anyhow::bail!(
                "existing extension contains a special file: {}",
                child.display()
            );
        }
    }
    Ok(())
}

fn destination_exists_and_is_safe(dest: &std::path::Path, path_is_stable: bool) -> Result<bool> {
    if !path_is_stable {
        crate::config::private_io::reject_symlink_components(dest)?;
    }
    let metadata = match std::fs::symlink_metadata(dest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect extension destination {}", dest.display())
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "extension destination is not a real directory: {}",
            dest.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!(
                "extension destination is not owned by the current user: {}",
                dest.display()
            );
        }
    }
    let mut budget = ExtensionCopyBudget::default();
    budget.visit(dest)?;
    validate_extension_tree(dest, &mut budget, 0, path_is_stable)?;
    Ok(true)
}

struct ExtensionStage {
    path: PathBuf,
    armed: bool,
}

impl ExtensionStage {
    fn new(parent: &std::path::Path) -> Result<Self> {
        for _ in 0..16 {
            let name = format!("{EXTENSION_STAGE_PREFIX}{}", uuid::Uuid::new_v4());
            let path = parent.join(name);
            match create_private_directory_new(&path) {
                Ok(()) => return Ok(Self { path, armed: true }),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        anyhow::bail!("failed to allocate a unique extension staging directory")
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.armed {
            std::fs::remove_dir_all(&self.path).with_context(|| {
                format!("failed to remove extension stage {}", self.path.display())
            })?;
            self.armed = false;
        }
        Ok(())
    }
}

impl Drop for ExtensionStage {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(target_os = "linux")]
fn install_staged_extension(
    parent: &std::fs::File,
    _parent_path: &std::path::Path,
    stage_name: &std::ffi::OsStr,
    dest_name: &std::ffi::OsStr,
    dest_exists: bool,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let stage_name = std::ffi::CString::new(stage_name.as_bytes())
        .context("extension staging name contains an embedded NUL")?;
    let dest_name = std::ffi::CString::new(dest_name.as_bytes())
        .context("extension destination name contains an embedded NUL")?;
    let flags = if dest_exists {
        libc::RENAME_EXCHANGE
    } else {
        libc::RENAME_NOREPLACE
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            stage_name.as_ptr(),
            parent.as_raw_fd(),
            dest_name.as_ptr(),
            flags,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            if dest_exists {
                "failed to atomically exchange the staged and installed extensions"
            } else {
                "failed to atomically install the staged extension without overwriting a concurrent destination"
            }
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn install_staged_extension(
    _parent: &std::fs::File,
    parent_path: &std::path::Path,
    stage_name: &std::ffi::OsStr,
    dest_name: &std::ffi::OsStr,
    dest_exists: bool,
) -> Result<()> {
    let parent = _parent
        .metadata()
        .context("failed to inspect extension parent")?;
    if !parent.is_dir() {
        anyhow::bail!("extension parent is no longer a directory");
    }
    let stage = parent_path.join(stage_name);
    let dest = parent_path.join(dest_name);
    if !dest_exists {
        if std::fs::symlink_metadata(&dest).is_ok() {
            anyhow::bail!("extension destination appeared during installation");
        }
        return std::fs::rename(&stage, &dest).context("failed to install staged extension");
    }
    let backup = parent_path.join(format!(
        ".phoenix-extension-backup-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::rename(&dest, &backup)
        .context("failed to stage existing extension for replacement")?;
    if let Err(error) = std::fs::rename(&stage, &dest) {
        let restore = std::fs::rename(&backup, &dest);
        return match restore {
            Ok(()) => Err(error).context("failed to install staged extension; prior extension restored"),
            Err(restore) => Err(anyhow::anyhow!(
                "failed to install staged extension ({error}); also failed to restore prior extension ({restore})"
            )),
        };
    }
    // Match Linux EXCHANGE semantics: the caller's guarded stage path owns
    // the prior tree and performs the bounded cleanup.
    std::fs::rename(&backup, &stage)
        .context("extension installed but prior extension could not be moved into cleanup stage")
}

/// Copy and install the GNOME extension as one bounded transaction. All source
/// bytes land in an owner-only sibling directory first. Linux then uses
/// renameat2(NO_REPLACE/EXCHANGE), so a failed copy or install leaves an
/// existing good extension untouched and a concurrent symlink is never
/// followed as an overwrite target.
pub(super) fn copy_dir_recursive(source: &PathBuf, dest: &PathBuf) -> Result<()> {
    crate::config::private_io::reject_symlink_components(source)?;
    let source_metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("failed to inspect extension source {}", source.display()))?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        anyhow::bail!(
            "extension source is not a real directory: {}",
            source.display()
        );
    }

    let dest_name = dest
        .file_name()
        .and_then(|name| checked_entry_name(name).ok())
        .context("extension destination must have one safe final component")?
        .to_owned();
    let dest_parent = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("extension destination must have a parent directory")?;
    crate::config::private_io::prepare_private_parent(dest)?;
    let parent_directory = open_extension_directory(dest_parent, false)?;
    let (stable_parent, parent_is_stable) = stable_directory_path(dest_parent, &parent_directory);
    let stable_dest = stable_parent.join(&dest_name);
    let _ = destination_exists_and_is_safe(&stable_dest, parent_is_stable)?;

    let mut stage = ExtensionStage::new(&stable_parent)?;
    let mut budget = ExtensionCopyBudget::default();
    budget.visit(source)?;
    copy_extension_tree(source, &stage.path, &mut budget, 0, false)?;

    // Re-check after the potentially long copy. renameat2 still provides the
    // final no-clobber/exchange guarantee if another process races this check.
    let dest_exists = destination_exists_and_is_safe(&stable_dest, parent_is_stable)?;
    let stage_name = stage
        .path
        .file_name()
        .context("extension stage lost its filename")?;
    install_staged_extension(
        &parent_directory,
        &stable_parent,
        stage_name,
        &dest_name,
        dest_exists,
    )?;
    if dest_exists {
        // EXCHANGE left the old installation at the private staging name.
        stage
            .cleanup()
            .context("extension installed, but prior installation cleanup failed")?;
    } else {
        stage.disarm();
    }
    parent_directory
        .sync_all()
        .context("extension installed, but its parent directory could not be synced")
}

#[cfg(test)]
mod extension_copy_tests {
    use super::*;

    fn make_source(root: &std::path::Path) -> PathBuf {
        let source = root.join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("metadata.json"), b"{\"uuid\":\"test\"}").unwrap();
        source
    }

    fn assert_no_stages(parent: &std::path::Path) {
        let stages = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(EXTENSION_STAGE_PREFIX)
            })
            .count();
        assert_eq!(stages, 0);
    }

    #[test]
    fn extension_budget_enforces_node_and_total_byte_caps() {
        let path = std::path::Path::new("entry");
        let mut nodes = ExtensionCopyBudget::default();
        for _ in 0..EXTENSION_MAX_NODES {
            nodes.visit(path).unwrap();
        }
        assert!(nodes.visit(path).is_err());

        let mut bytes = ExtensionCopyBudget::default();
        for _ in 0..(EXTENSION_MAX_TOTAL_BYTES / EXTENSION_MAX_FILE_BYTES) {
            bytes.add_file(path, EXTENSION_MAX_FILE_BYTES).unwrap();
        }
        assert!(bytes.add_file(path, 1).is_err());
    }

    #[test]
    fn extension_install_creates_a_new_tree_without_staging_residue() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        std::fs::write(source.join("extension.js"), b"new").unwrap();
        let dest = dir.path().join("installed");

        copy_dir_recursive(&source, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("extension.js")).unwrap(), b"new");
        assert_no_stages(dir.path());
    }

    #[test]
    fn extension_install_replaces_a_complete_tree_and_removes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        std::fs::create_dir(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/extension.js"), b"new").unwrap();
        let dest = dir.path().join("installed");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("stale.js"), b"old").unwrap();

        copy_dir_recursive(&source, &dest).unwrap();
        assert_eq!(
            std::fs::read(dest.join("nested/extension.js")).unwrap(),
            b"new"
        );
        assert!(!dest.join("stale.js").exists());
        assert_no_stages(dir.path());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(dest.join("nested/extension.js"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn oversized_source_preserves_existing_installation() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        let oversized = std::fs::File::create(source.join("oversized.bin")).unwrap();
        oversized.set_len(EXTENSION_MAX_FILE_BYTES + 1).unwrap();
        let dest = dir.path().join("installed");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("known-good"), b"old").unwrap();

        let error = copy_dir_recursive(&source, &dest).unwrap_err();
        assert!(format!("{error:#}").contains("too large"));
        assert_eq!(std::fs::read(dest.join("known-good")).unwrap(), b"old");
        assert_no_stages(dir.path());
    }

    #[test]
    fn excessive_depth_preserves_existing_installation() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        let mut nested = source.clone();
        for index in 0..=EXTENSION_MAX_DEPTH {
            nested = nested.join(format!("d{index}"));
            std::fs::create_dir(&nested).unwrap();
        }
        let dest = dir.path().join("installed");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("known-good"), b"old").unwrap();

        let error = copy_dir_recursive(&source, &dest).unwrap_err();
        assert!(format!("{error:#}").contains("exceeded depth"));
        assert_eq!(std::fs::read(dest.join("known-good")).unwrap(), b"old");
        assert_no_stages(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_and_fifo_are_rejected_without_partial_install() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, source.join("escape")).unwrap();
        let dest = dir.path().join("installed");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("known-good"), b"old").unwrap();
        assert!(copy_dir_recursive(&source, &dest).is_err());
        assert_eq!(std::fs::read(dest.join("known-good")).unwrap(), b"old");

        std::fs::remove_file(source.join("escape")).unwrap();
        let fifo = source.join("blocked-pipe");
        let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(copy_dir_recursive(&source, &dest).is_err());
        assert_eq!(std::fs::read(dest.join("known-good")).unwrap(), b"old");
        assert_no_stages(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn destination_symlinks_never_redirect_the_install() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"untouched").unwrap();
        let dest = dir.path().join("installed");
        symlink(&outside, &dest).unwrap();

        assert!(copy_dir_recursive(&source, &dest).is_err());
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert!(!outside.join("metadata.json").exists());
        assert_no_stages(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_existing_destination_is_rejected_and_preserved() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = make_source(dir.path());
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"untouched").unwrap();
        let dest = dir.path().join("installed");
        std::fs::create_dir(&dest).unwrap();
        symlink(&outside, dest.join("linked-file")).unwrap();

        assert!(copy_dir_recursive(&source, &dest).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        assert!(std::fs::symlink_metadata(dest.join("linked-file"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_no_stages(dir.path());
    }
}
