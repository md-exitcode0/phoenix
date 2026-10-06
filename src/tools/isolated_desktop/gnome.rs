//! A private compositor, session bus and cursor for one existing DesktopScope.
//! This reuses the desktop registry and leases; it is not a second scheduler.
use super::*;

const EXTENSION: &str = "phoenix-cursor@phoenix.dev";
const FILES: &[(&str, &str)] = &[
    ("extension.js", include_str!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/extension.js")),
    ("cursor-art.mjs", include_str!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-art.mjs")),
    ("cursor-input.mjs", include_str!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor-input.mjs")),
    ("stylesheet.css", include_str!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/stylesheet.css")),
    ("metadata.json", include_str!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/metadata.json")),
];
/// The neon arrow art the extension paints as the agent pointer.
const CURSOR_ART: &[u8] = include_bytes!("../../../desktop/gnome-extension/phoenix-cursor@phoenix.dev/cursor.png");

#[derive(Deserialize)]
struct Descriptor {
    version: u8,
    scope_key: String,
    runtime_dir: PathBuf,
    session_bus: String,
    wayland_display: String,
    display: String,
    xauthority: PathBuf,
}

impl Descriptor {
    fn environment(self, scope: &DesktopScope, runtime: &Path) -> Result<DesktopEnvironment> {
        anyhow::ensure!(self.version == 1 && self.scope_key == scope.key && self.runtime_dir == runtime,
            "native compositor descriptor belongs to another desktop");
        anyhow::ensure!(self.wayland_display == "phoenix-agent" && self.session_bus.starts_with("unix:path=")
            && self.display.starts_with(':') && self.display[1..].parse::<u32>().is_ok()
            && self.xauthority.starts_with(runtime), "invalid native compositor endpoints");
        anyhow::ensure!(runtime.join(&self.wayland_display).exists() && self.xauthority.is_file(),
            "native compositor published endpoints before they became ready");
        Ok(DesktopEnvironment {display:self.display, xauthority:self.xauthority,
            runtime_dir:runtime.to_path_buf(), scope_key:scope.key.clone(), visible:false,
            native:Some(NativeDesktopEnvironment {wayland_display:self.wayland_display, session_bus:self.session_bus})})
    }
}

pub(super) fn start(scope: &DesktopScope) -> Result<DesktopState> {
    for binary in ["gnome-shell", "dbus-run-session", "gdbus"] {
        anyhow::ensure!(executable_in_path(binary), "native agent desktop needs {binary}");
    }
    let root = desktop_root(scope)?;
    let runtime = root.join("runtime");
    let data = root.join("data");
    let config = root.join("config");
    let cache = root.join("cache");
    for path in [&runtime,&data,&config,&cache] {
        crate::config::private_io::prepare_phoenix_directory(path)?;
    }
    let extension = data.join("gnome-shell/extensions").join(EXTENSION);
    for (name, content) in FILES {
        crate::config::private_io::atomic_write_private(&extension.join(name),content.as_bytes())?;
    }
    crate::config::private_io::atomic_write_private(&extension.join("cursor.png"),CURSOR_ART)?;
    crate::config::private_io::atomic_write_private(
        &config.join("glib-2.0/settings/keyfile"),
        b"[org/gnome/shell]\nenabled-extensions=['phoenix-cursor@phoenix.dev']\ndisable-user-extensions=false\n\n[org/gnome/desktop/a11y/keyboard]\nmousekeys-enable=false\n")?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(root.join("compositor.log"))?;
    secure_file(&root.join("compositor.log"))?;
    let mut command = Command::new("dbus-run-session");
    command.args(["--", "gnome-shell", "--headless", "--wayland", "--virtual-monitor",
        &format!("{DEFAULT_WIDTH}x{DEFAULT_HEIGHT}"), "--wayland-display", "phoenix-agent"])
        .env("XDG_RUNTIME_DIR",&runtime).env("XDG_DATA_HOME",&data)
        .env("XDG_CONFIG_HOME",&config).env("XDG_CACHE_HOME",&cache)
        // GIO services remain available, but an agent's disposable runtime must
        // not acquire a GVFS FUSE mount that outlives the private session bus.
        .env("GVFS_DISABLE_FUSE", "1")
        .env("GSETTINGS_BACKEND","keyfile").env("PHOENIX_DESKTOP_SCOPE",scope.key())
        .env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").env_remove("XAUTHORITY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS").env_remove("DBUS_STARTER_ADDRESS")
        .env_remove("DBUS_STARTER_BUS_TYPE").env_remove("XDG_ACTIVATION_TOKEN")
        .stdin(Stdio::null()).stdout(Stdio::from(log.try_clone()?)).stderr(Stdio::from(log));
    let child = spawn_owned(&mut command,"agent compositor")?;
    let mut processes = vec![DesktopProcess {child,label:"agent compositor"}];
    let ready = (|| -> Result<DesktopEnvironment> {
        let descriptor = runtime.join("phoenix-desktop.json");
        let deadline = Instant::now()+Duration::from_secs(15);
        loop {
            anyhow::ensure!(processes[0].child.try_wait()?.is_none(),"agent compositor exited; inspect {}",root.join("compositor.log").display());
            if let Some(bytes) = crate::config::private_io::read_private_file_limited(&descriptor,16*1024)? {
                return serde_json::from_slice::<Descriptor>(&bytes)?.environment(scope,&runtime);
            }
            anyhow::ensure!(Instant::now()<deadline,"agent compositor did not become ready; inspect {}",root.join("compositor.log").display());
            std::thread::sleep(READY_POLL);
        }
    })();
    let environment = match ready {
        Ok(environment) => environment,
        Err(error) => {terminate_processes(&mut processes);return Err(error);}
    };
    let lease = LeaseRecord {version:1,owner_pid:std::process::id(),
        owner_start_ticks:current_process_start_ticks(),server_pid:processes[0].child.id(),
        server_start_ticks:process_start_ticks(processes[0].child.id()),
        native_runtime:Some(runtime.clone()),server:"dbus-run-session".into(),
        display:environment.display.clone(),xauthority:environment.xauthority.to_string_lossy().into_owned()};
    if let Err(error) = write_lease(&root,&lease) {
        terminate_processes(&mut processes);
        return Err(error);
    }
    Ok(DesktopState {label:scope.label.clone(),owner_agent_id:scope.owner_agent_id.clone(),observation:None,environment,root,processes,ephemeral:scope.ephemeral,
        browser_instance:scope.browser_instance.clone(),last_used:Instant::now()})
}

#[cfg(target_os="linux")]
fn recorded_identity_matches(lease: &LeaseRecord) -> bool {
    let Some(runtime) = lease.native_runtime.as_ref() else {return false;};
    let Some(root) = runtime.parent() else {return false;};
    let Some(scope) = root.file_name().and_then(|value| value.to_str()) else {return false;};
    if lease.server != "dbus-run-session" || !valid_desktop_directory_name(scope)
        || root.parent() != Some(crate::config::phoenix_home().join("desktops").as_path())
        || runtime.file_name().and_then(|value|value.to_str()) != Some("runtime")
        || !process_matches_start(lease.server_pid,lease.server_start_ticks) {
        return false;
    }
    let cmd = std::fs::read(format!("/proc/{}/cmdline",lease.server_pid)).unwrap_or_default();
    let args = cmd.split(|b|*b==0).collect::<Vec<_>>();
    let env = std::fs::read(format!("/proc/{}/environ",lease.server_pid)).unwrap_or_default();
    let variables = env.split(|b|*b==0).collect::<Vec<_>>();
    args.iter().any(|arg|*arg==b"gnome-shell") && args.iter().any(|arg|*arg==b"--headless")
        && args.iter().any(|arg|*arg==b"phoenix-agent")
        && variables.iter().any(|value|*value==format!("XDG_RUNTIME_DIR={}",runtime.display()).as_bytes())
        && variables.iter().any(|value|*value==format!("PHOENIX_DESKTOP_SCOPE={scope}").as_bytes())
        && unsafe {libc::getpgid(lease.server_pid as i32)} == lease.server_pid as i32
}

pub(super) fn terminate_recorded(lease: &LeaseRecord) -> bool {
    #[cfg(target_os="linux")]
    {
        let members = match recorded_group_members(lease) {
            Ok(members)=>members,
            Err(error)=>{tracing::warn!("native desktop recovery refused: {error:#}");return false;}
        };
        if members.is_empty() {return true;}
        let group = lease.server_pid as i32;
        // The group belongs to the recorded wrapper and includes its bus and
        // compositor. Never signal a PID selected only by executable name.
        unsafe {libc::kill(-group,libc::SIGTERM);}
        let deadline = Instant::now()+Duration::from_secs(1);
        while Instant::now()<deadline {
            match recorded_group_members(lease) {
                Ok(members) if members.is_empty() => return true,
                // During shutdown a sandbox parent can exit just before its
                // child. Keep observing; do not send further signals without
                // re-establishing ownership, or erase live state prematurely.
                _ => {},
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if recorded_group_members(lease).is_ok_and(|members|!members.is_empty()) {
            unsafe {libc::kill(-group,libc::SIGKILL);}
        }
        let deadline = Instant::now()+Duration::from_secs(1);
        while Instant::now()<deadline {
            if recorded_group_members(lease).is_ok_and(|members|members.is_empty()) {return true;}
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }
    #[cfg(not(target_os="linux"))]
    {let _=lease;false}
}

#[cfg(target_os="linux")]
fn recorded_group_members(lease: &LeaseRecord) -> Result<Vec<u32>> {
    let runtime = lease.native_runtime.as_ref().context("missing native runtime")?;
    let root = runtime.parent().context("invalid native root")?;
    let scope = root.file_name().and_then(|value|value.to_str()).context("invalid native scope")?;
    anyhow::ensure!(lease.server=="dbus-run-session" && lease.server_pid>1
        && lease.server_start_ticks.is_some() && valid_desktop_directory_name(scope)
        && root.parent()==Some(crate::config::phoenix_home().join("desktops").as_path())
        && runtime.file_name().and_then(|value|value.to_str())==Some("runtime"),"invalid native lease");
    if process_alive(lease.server_pid) {
        anyhow::ensure!(process_start_ticks(lease.server_pid)==lease.server_start_ticks,"native launcher PID was reused");
    }
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name|name.parse::<u32>().ok()) else {continue;};
        if unsafe {libc::getpgid(pid as i32)} != lease.server_pid as i32 {continue;}
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {continue;};
        if stat.rsplit_once(") ").is_some_and(|(_,rest)|rest.starts_with('Z')) {continue;}
        let env = match std::fs::read(entry.path().join("environ")) {
            Ok(env)=>env,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>continue,
            Err(error)=>return Err(error.into()),
        };
        let variables=env.split(|b|*b==0).collect::<Vec<_>>();
        let matching_scope = variables.iter().any(|value|*value==format!("PHOENIX_DESKTOP_SCOPE={scope}").as_bytes());
        let inherited_scope = !variables.iter().any(|value|value.starts_with(b"PHOENIX_DESKTOP_SCOPE="))
            && scoped_ancestor(pid,lease.server_pid,runtime,scope);
        anyhow::ensure!(variables.iter().any(|value|*value==format!("XDG_RUNTIME_DIR={}",runtime.display()).as_bytes())
            && (matching_scope || inherited_scope),
            "native group contains process {pid} without matching scope identity");
        members.push(pid);
    }
    Ok(members)
}

#[cfg(target_os="linux")]
fn scoped_ancestor(mut pid:u32,group:u32,runtime:&Path,scope:&str)->bool {
    // Sandboxed image decoders retain the runtime but intentionally filter
    // custom variables. Accept only a still-live ancestor in this exact group
    // with both original identities; never use executable names as evidence.
    for _ in 0..16 {
        let Ok(stat)=std::fs::read_to_string(format!("/proc/{pid}/stat")) else {return false;};
        let Some(parent)=stat.rsplit_once(") ").and_then(|(_,rest)|rest.split_whitespace().nth(1))
            .and_then(|value|value.parse::<u32>().ok()) else {return false;};
        if parent<=1 || parent==pid || unsafe {libc::getpgid(parent as i32)}!=group as i32 {return false;}
        let Ok(env)=std::fs::read(format!("/proc/{parent}/environ")) else {return false;};
        let vars=env.split(|b|*b==0).collect::<Vec<_>>();
        if vars.iter().any(|value|*value==format!("XDG_RUNTIME_DIR={}",runtime.display()).as_bytes())
            && vars.iter().any(|value|*value==format!("PHOENIX_DESKTOP_SCOPE={scope}").as_bytes()) {return true;}
        pid=parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "starts two private GNOME compositors simultaneously and captures both"]
    fn native_desktops_start_concurrently_and_keep_independent_input() {
        let home=tempfile::tempdir().unwrap();
        let _home=crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scopes=(0..2).map(|i|DesktopScope::agent("native-overlap",&format!("actor-{i}"),None).unwrap()).collect::<Vec<_>>();
        struct Cleanup(Vec<DesktopScope>);
        impl Drop for Cleanup {fn drop(&mut self){for scope in &self.0 {discard_scope(scope);}}}
        let _cleanup=Cleanup(scopes.clone());
        let barrier=Arc::new(std::sync::Barrier::new(2));
        let origin=Instant::now();
        let workers=scopes.iter().cloned().enumerate().map(|(i,scope)| {
            let barrier=barrier.clone();
            std::thread::spawn(move||->Result<_>{
                barrier.wait();
                let mut begin=0;let mut ready=0;
                let environment=ensure_with_start(&scope,|scope| {
                    begin=origin.elapsed().as_millis();
                    let state=start(scope)?;
                    ready=origin.elapsed().as_millis();
                    Ok(state)
                })?;
                with_scope(Some(scope),||->Result<()> {
                    use crate::tools::computer_use as computer;
                    computer::move_cursor(computer::MoveInput{x:300+i as i64*200,y:250,duration_ms:0})?;
                    let screenshot=computer::screenshot(computer::ScreenshotInput{})?;
                    anyhow::ensure!(screenshot.content.contains(&format!("{}x{}",DEFAULT_WIDTH,DEFAULT_HEIGHT)),"wrong native capture dimensions");
                    Ok(())
                })?;
                Ok((environment,begin,ready))
            })
        }).collect::<Vec<_>>();
        let results=workers.into_iter().map(|worker|worker.join().unwrap()).collect::<Result<Vec<_>>>().unwrap();
        assert_ne!(results[0].0.display,results[1].0.display);
        assert_ne!(results[0].0.runtime_dir,results[1].0.runtime_dir);
        assert!(results.iter().all(|(env,_,_)|env.native.is_some()));
        let latest_start=results.iter().map(|(_,start,_)|*start).max().unwrap();
        let earliest_ready=results.iter().map(|(_,_,ready)|*ready).min().unwrap();
        assert!(latest_start<earliest_ready,"real compositor startup intervals did not overlap");
        for (i,scope) in scopes.iter().enumerate() {
            with_scope(Some(scope.clone()),|| {
                let status=crate::tools::computer_use::status(crate::tools::computer_use::StatusInput{}).unwrap();
                let begin=status.content.find('{').unwrap();
                let end=status.content.rfind('}').unwrap();
                let state:serde_json::Value=serde_json::from_str(&status.content[begin..=end]).unwrap();
                assert_eq!(state["agent_cursor"]["x"],300+i*200,"cursor state not retained independently");
                assert_eq!(state["agent_cursor"]["y"],250);
            });
        }
        eprintln!("NATIVE_STARTUP_OVERLAP {}",serde_json::json!({"elapsed_ms":origin.elapsed().as_millis(),
            "intervals_ms":results.iter().map(|(_,begin,ready)|serde_json::json!({"begin":begin,"ready":ready})).collect::<Vec<_>>(),
            "independent_displays":true,"native_captures":2}));
    }

    #[test]
    #[ignore = "child-only crash fixture; invoked by native_owner_crash_reclaims_scope"]
    fn native_owner_crash_child() {
        let path = std::env::var_os("PHOENIX_NATIVE_CRASH_HOME").expect("parent fixture home");
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(Path::new(&path));
        let scope = DesktopScope::agent("crash-recovery","native",None).unwrap();
        let state = start(&scope).unwrap();
        std::mem::forget(state);
        unsafe {libc::kill(libc::getpid(),libc::SIGKILL);}
        panic!("SIGKILL did not terminate the fixture");
    }

    #[cfg(target_os="linux")]
    #[test]
    #[ignore = "kills an owned test process after it starts a private GNOME compositor"]
    fn native_owner_crash_reclaims_scope() {
        use std::os::unix::process::ExitStatusExt;
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope = DesktopScope::agent("crash-recovery","native",None).unwrap();
        let root = desktop_root(&scope).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","tools::isolated_desktop::gnome::tests::native_owner_crash_child","--ignored","--nocapture"])
            .env("PHOENIX_NATIVE_CRASH_HOME",home.path()).stdout(Stdio::null()).stderr(Stdio::inherit());
        let status = command.status().unwrap();
        assert_eq!(status.signal(),Some(libc::SIGKILL),"crash fixture did not reach the kill boundary");
        let lease: LeaseRecord = serde_json::from_slice(&std::fs::read(root.join(LEASE_FILE)).unwrap()).unwrap();
        let recovered = terminate_recorded_server(&lease);
        eprintln!("NATIVE_MEMBERS {:?}",recorded_group_members(&lease));
        let group_gone = recorded_group_members(&lease).is_ok_and(|members|members.is_empty());
        eprintln!("NATIVE_OWNER_CRASH group_gone={group_gone} pid={} root={}",lease.server_pid,root.display());
        // Do not erase evidence while an uncertain native descendant survives.
        if !recovered || !group_gone {
            let retained = home.keep();
            panic!("native compositor descendants survived owner death; evidence retained at {}",retained.display());
        }
        reclaim_stale_scopes();
        assert!(!root.exists(),"dead native scope was not reclaimed");
    }

    #[test]
    #[ignore = "starts private GNOME and activates its GVFS service"]
    fn owned_gnome_cleanup_removes_root_after_gvfs_activation() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope = DesktopScope::agent("native-gvfs-cleanup", "native", None).unwrap();
        let state = start(&scope).unwrap();
        let root = state.root.clone();
        let mut activate = Command::new("gdbus");
        state.environment.apply_to_command(&mut activate);
        activate.args(["call", "--session", "--dest", "org.freedesktop.DBus",
            "--object-path", "/org/freedesktop/DBus", "--method",
            "org.freedesktop.DBus.StartServiceByName", "org.gtk.vfs.Daemon", "0"]);
        let activation = activate.output();
        // Let the activated daemon finish its optional FUSE startup before
        // killing the owned bus/compositor group and checking actual removal.
        std::thread::sleep(Duration::from_secs(1));
        let mount_path = root.join("runtime/gvfs").to_string_lossy().into_owned();
        let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let private_fuse_mounted = mounts.lines().any(|line| line.split_whitespace().nth(4) == Some(mount_path.as_str()));
        cleanup_state(state);
        assert!(!private_fuse_mounted, "private runtime acquired a FUSE mount");
        assert!(activation.unwrap().status.success(), "private GVFS activation failed");
        assert!(!root.exists(), "private desktop root survived cleanup: {}", root.display());
    }

    #[test]
    #[ignore = "starts an actual private GNOME compositor and its cursor"]
    fn owned_gnome_scope_routes_cursor_and_screenshot_to_its_private_bus() {
        let home = tempfile::tempdir().unwrap();
        let _home = crate::config::test_env::PhoenixHomeGuard::set_private(home.path());
        let scope = DesktopScope::agent("native-scope-test","native",None).unwrap();
        let state = start(&scope).unwrap();
        let mut lease: LeaseRecord = serde_json::from_slice(&std::fs::read(state.root.join(LEASE_FILE)).unwrap()).unwrap();
        assert_eq!(lease.native_runtime.as_ref(),Some(&state.environment.runtime_dir));
        #[cfg(target_os="linux")]
        {
            assert!(recorded_identity_matches(&lease));
            let original = lease.server_start_ticks;
            lease.server_start_ticks=original.map(|ticks|ticks+1);
            assert!(!recorded_identity_matches(&lease));
            assert!(!terminate_recorded(&lease));
            lease.server_start_ticks=original;
            assert!(recorded_identity_matches(&lease));
        }
        let pid = state.processes[0].child.id();
        let environment = state.environment.clone();
        let fixture_root = environment.runtime_dir.parent().unwrap().to_path_buf();
        let mut fixture_command = Command::new("gjs");
        fixture_command.arg("-m")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/cursor-native-input-fixture.mjs"))
            .arg(&fixture_root).env("PHOENIX_HOME",home.path()).stdout(Stdio::null()).stderr(Stdio::null());
        environment.apply_to_command(&mut fixture_command);
        let mut fixture = fixture_command.spawn().unwrap();
        let original_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
        DESKTOPS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().insert(scope.key.clone(),state);
        let view=existing_desktops().into_iter().find(|view|view.scope_key==scope.key).unwrap();
        assert_eq!(view.backend,"gnome");
        assert_eq!(view.label,scope.label);
        assert!(view.running);
        assert!(!view.in_use);
        let viewer_lease=acquire_lease(&scope);
        assert!(existing_desktops().into_iter().find(|view|view.scope_key==scope.key).unwrap().in_use);
        let result = with_scope(Some(scope.clone()), || -> Result<()> {
            use crate::tools::computer_use as computer;
            assert_eq!(computer::detect_backend()?,computer::Backend::GnomeBridge);
            computer::move_cursor(computer::MoveInput{x:410,y:320,duration_ms:0})?;
            // Exercise signed positional parameters through the production
            // gdbus subprocess, including the upward scroll used by Blender.
            for (dx,dy) in [(0,3),(0,-3),(-2,0),(2,0)] {
                computer::scroll(computer::ScrollInput{dx,dy})?;
            }
            let status = computer::status(computer::StatusInput{})?;
            anyhow::ensure!(status.content.contains("410") && status.content.contains("320"),"cursor did not arrive: {}",status.content);
            let shot = computer::screenshot(computer::ScreenshotInput{})?;
            eprintln!("OWNED_NATIVE {}",shot.content);
            anyhow::ensure!(shot.content.contains(&format!("{}x{}",DEFAULT_WIDTH,DEFAULT_HEIGHT)),"native screenshot has incorrect dimensions");
            let observation=latest_observation(scope.key(),None)?;
            assert_eq!(observation.kind.as_deref(),Some("desktop"));
            assert_eq!(observation.width,Some(DEFAULT_WIDTH));
            assert!(observation.data_url.as_deref().is_some_and(|data|data.starts_with("data:image/png;base64,iVBOR")));
            assert!(latest_observation(scope.key(),observation.captured_at_ms)?.data_url.is_none());
            assert!(latest_observation("not-an-existing-desktop",None).is_err());
            let deadline = Instant::now()+Duration::from_secs(5);
            let id = loop {
                let listing = computer::list_windows(computer::ListWindowsInput{})?;
                let json = listing.content.find('{').context("window listing has no JSON")?;
                let listing: serde_json::Value = serde_json::from_str(&listing.content[json..])?;
                if let Some(id) = listing["windows"].as_array().and_then(|windows| windows.iter()
                    .find(|window| window["title"]=="Phoenix native input acceptance"))
                    .and_then(|window| window["id"].as_i64()) { break id; }
                anyhow::ensure!(fixture.try_wait()?.is_none(),"native input fixture exited");
                anyhow::ensure!(Instant::now()<deadline,"native fixture window did not appear");
                std::thread::sleep(READY_POLL);
            };
            let expected = "Phoenix café — 日本語 😀";
            computer::window_act(computer::WindowActInput{id,
                actions:serde_json::json!([{"type":"type","text":expected},
                    {"type":"key","combo":"num1"},{"type":"key","combo":"numdecimal"}]),capture:true})?;
            let newer=latest_observation(scope.key(),observation.captured_at_ms)?;
            assert_eq!(newer.kind.as_deref(),Some("window"));
            assert!(newer.data_url.is_some());
            assert!(newer.captured_at_ms>observation.captured_at_ms);
            let state_path = fixture_root.join("input-state.json");
            let deadline = Instant::now()+Duration::from_secs(3);
            loop {
                let state: serde_json::Value = serde_json::from_slice(&std::fs::read(&state_path)?)?;
                if state["keycodes"].as_array().is_some_and(|codes|codes.contains(&serde_json::json!(87))&&codes.contains(&serde_json::json!(91))) {
                    anyhow::ensure!(state["text"].as_str().is_some_and(|text|text.starts_with(expected)),"keypad input corrupted existing text: {state}");
                    break;
                }
                anyhow::ensure!(Instant::now()<deadline,"native application text differs: {state}");
                std::thread::sleep(READY_POLL);
            }
            let closed=computer::window_act(computer::WindowActInput{id,
                actions:serde_json::json!([{"type":"key","combo":"alt+f4"},{"type":"wait","ms":1200}]),capture:true})?;
            anyhow::ensure!(closed.content.contains("previous window was not refocused or recaptured"),
                "terminal transition must not attempt to capture the closed target: {}",closed.content);
            let deadline=Instant::now()+Duration::from_secs(3);
            loop {
                let listing=computer::list_windows(computer::ListWindowsInput{})?;
                let begin=listing.content.find('{').context("window listing has no JSON")?;
                let state:serde_json::Value=serde_json::from_str(&listing.content[begin..])?;
                if !state["windows"].as_array().context("window list missing")?.iter().any(|window|window["id"]==id) {break;}
                anyhow::ensure!(Instant::now()<deadline,"closed window remained live");
                std::thread::sleep(READY_POLL);
            }
            Ok(())
        });
        let _ = fixture.kill(); let _ = fixture.wait();
        drop(viewer_lease);
        discard_scope(&scope);
        assert!(!existing_desktops().iter().any(|view|view.scope_key==scope.key));
        assert!(!process_alive(pid));
        assert_eq!(original_bus,std::env::var_os("DBUS_SESSION_BUS_ADDRESS"));
        result.unwrap();
    }
}
