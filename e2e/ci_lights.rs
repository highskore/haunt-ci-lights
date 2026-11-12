//! End to end against a real haunt core. Copy into haunt's `cli/tests/` and run
//! `cargo test -p haunt-cli --features plugins --test ci_lights` (see README).
#![cfg(feature = "plugins")]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haunt_core::plugin_store::{PluginSet, Store, attach};
use haunt_core::plugins::PluginHost;
use haunt_core::{Core, SurfaceEnv};

const ID: &str = "dev.haunt.ci-lights";

fn repo() -> PathBuf {
    std::env::var_os("HAUNT_PLUGINS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../haunt-plugins"))
        .join("haunt-ci-lights")
}

fn send(sock: &mut UnixStream, env: &SurfaceEnv, nonce: bool, cwd: &str, rest: &str) {
    let nonce = if nonce { format!(r#","nonce":"{}""#, env.nonce) } else { String::new() };
    let line = format!(r#"{{"v":1,"agent":"claude","surface_id":"{}"{nonce},"session_id":"s1","cwd":"{cwd}",{rest}}}"#, env.surface_id);
    sock.write_all(format!("{line}\n").as_bytes()).unwrap();
}

/// The plugin's log lines, waiting until `want` of them have come.
fn logs(set: &Mutex<PluginSet>, want: usize) -> Vec<String> {
    let t = Instant::now();
    let mut out = Vec::new();
    while out.len() < want {
        out.extend(set.lock().unwrap().take_logs().into_iter().filter(|(id, _)| id == ID).map(|(_, l)| l));
        assert!(t.elapsed() < Duration::from_secs(20), "timed out waiting for {want} log lines: {out:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    out
}

const PUSH: &str = r#""event":"tool_ran","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"git push -u origin feat/auth"}"#;

#[test]
fn a_push_asks_gh_in_the_tabs_own_folder() {
    let dir = repo();
    if !dir.join("haunt-plugin.toml").is_file() {
        eprintln!("skipped: haunt-ci-lights not found; set HAUNT_PLUGINS_DIR");
        return;
    }
    let base = PathBuf::from(format!("/tmp/haunt-fp-ci-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (home, work) = (base.join("haunt"), base.join("user"));
    let api = work.join("api");
    std::fs::create_dir_all(work.join(".hidden")).unwrap();
    std::fs::create_dir_all(&api).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&api).status().unwrap().success());
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .current_dir(&dir)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap();
    assert!(status.success());
    let o = Command::new(env!("CARGO_BIN_EXE_haunt")).args(["plugin", "install", dir.to_str().unwrap(), "--yes"]).env("HAUNT_HOME", &home).output().unwrap();
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));

    // A tab's folder counts only strictly inside $HOME; gh then finds no login or remote here, offline.
    unsafe { std::env::set_var("HOME", &work) };
    let core = Core::new(&home);
    core.start().unwrap();
    let api_s = api.to_str().unwrap();
    let hidden_s = work.join(".hidden");
    let hidden_s = hidden_s.to_str().unwrap();
    let (tab, forged, hidden) = (core.register_surface(api_s).unwrap(), core.register_surface(api_s).unwrap(), core.register_surface(hidden_s).unwrap());
    let set = Store::open(&home).unwrap().load_all(&PluginHost::new().unwrap(), Some(&core.handle())).unwrap();
    assert!(set.errors().is_empty(), "{:?}", set.errors());
    let set = Arc::new(Mutex::new(set));
    attach(&core, &set);
    let decoration = |s: &str| core.snapshot().groups.iter().flat_map(|g| &g.rows).find(|r| r.surface_id == s).and_then(|r| r.decoration.clone());

    let mut sock = UnixStream::connect(&tab.socket_path).unwrap();
    send(&mut sock, &tab, true, api_s, r#""event":"tool_ran","tool_name":"Bash","tool_use_id":"t0","tool_input":{"command":"git status"}"#);
    send(&mut sock, &forged, false, api_s, PUSH);
    send(&mut sock, &tab, true, api_s, PUSH);
    let l = logs(&set, 1);
    assert!(l[0].contains("ci-lights: gh exited "), "gh ran in the tab's folder and failed there: {l:?}");
    assert_eq!(decoration(&tab.surface_id), None, "no PR, no light");

    send(&mut sock, &hidden, true, hidden_s, PUSH);
    let l = logs(&set, 1);
    assert!(l[0].contains("ci-lights: gh not run: "), "{l:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(logs(&set, 0).is_empty(), "the forged push and the plain command ran nothing");

    drop(core);
    let _ = std::fs::remove_dir_all(&base);
}
