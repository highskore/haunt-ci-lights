//! CI lights: after an agent runs `git push`, its row tracks the PR's checks (`CI 7/9`, `#142 ·
//! checks running`) and a notification comes only when they go red or all pass.
// wit-bindgen's flattened `notify` import has 14 params.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({ path: "wit/haunt-plugin.wit", world: "plugin" });

mod ci;

use std::collections::BTreeMap;
use std::sync::Mutex;

use ci::{Summary, Verdict};
use haunt::plugin::exec::{self, Command};
use haunt::plugin::link;
use haunt::plugin::log::log;
use haunt::plugin::notify::{self, Action};
use haunt::plugin::rows::{self, Decoration, Tone};
use haunt::plugin::types::LogLevel;
use haunt::plugin::timer;
use serde::Deserialize;

/// Checks that haven't shown up this long after a push are given up on.
const NO_CHECKS_S: i64 = 10 * 60;
/// Nor are checks still running after this long.
const GIVE_UP_S: i64 = 2 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Seconds between looks at the checks, 15 or more.
    pub poll_secs: i64,
    pub notify_failed: bool,
    pub notify_passed: bool,
    pub unverified: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { poll_secs: 30, notify_failed: true, notify_passed: true, unverified: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub name: String,
    pub pushed_s: i64,
    pub pr: Option<u64>,
    /// The PR's page, for the notification's Open PR.
    pub url: Option<String>,
    pub summary: Option<Summary>,
    /// Still asking gh; false once the checks settle or it gave up.
    pub polling: bool,
}

#[derive(Default)]
struct Live {
    settings: Settings,
    tabs: BTreeMap<String, Tab>,
    jobs: BTreeMap<u64, (String, Command)>,
    /// Per tab, gh's last complaint, so a failing poll is logged once rather than every time.
    complaints: BTreeMap<String, String>,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Live) -> R) -> R {
    f(LIVE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Live::default))
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn name_of(cwd: &str) -> String {
    cwd.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(cwd).to_owned()
}

/// `CI 7/9` in the label slot, orange when red, and `#142 · checks running` after the meta.
pub fn decoration(t: &Tab) -> Option<Decoration> {
    let (s, pr) = (t.summary.as_ref()?, t.pr?);
    let tone = if s.verdict == Verdict::Failed { Tone::Warn } else { Tone::Text };
    Some(Decoration { label: Some(s.label()), label_tone: tone, suffix: Some(s.suffix(pr)), suffix_tone: Tone::Dim, scroll: false, bar: None, spark: None })
}

/// `api · #142 checks failed` and its body, when the settings want a ping.
pub fn ping(t: &Tab, s: &Settings) -> Option<(String, String)> {
    let (sum, pr) = (t.summary.as_ref()?, t.pr?);
    let wanted = match sum.verdict {
        Verdict::Failed => s.notify_failed,
        Verdict::Passed => s.notify_passed,
        Verdict::Running => false,
    };
    let what = if sum.verdict == Verdict::Failed { "failed" } else { "passed" };
    wanted.then(|| (format!("{} · #{pr} checks {what}", t.name), sum.body()))
}

/// Logs what went wrong with gh for a tab once, until it changes or gh succeeds.
fn complain(l: &mut Live, surface: &str, what: Option<String>) {
    match what {
        Some(w) if l.complaints.get(surface) != Some(&w) => {
            log(LogLevel::Info, &format!("ci-lights: gh {w}"));
            l.complaints.insert(surface.to_owned(), w);
        }
        Some(_) => {}
        None => {
            l.complaints.remove(surface);
        }
    }
}

/// Open PR (when its page is on github.com, the manifest's one `open_url` domain), then Open agent.
pub fn actions(t: &Tab) -> Vec<Action> {
    let pr = t.url.as_ref().filter(|u| u.starts_with("https://github.com/") && u.len() <= 256);
    pr.map(|u| Action { id: u.clone(), title: "Open PR".into() }).into_iter().chain([Action { id: "open".into(), title: "Open agent".into() }]).collect()
}

fn run(l: &mut Live, surface: &str, c: Command) {
    match exec::run(surface, c) {
        Ok(job) => {
            l.jobs.insert(job, (surface.to_owned(), c));
        }
        Err(e) => complain(l, surface, Some(format!("not run: {e}"))),
    }
}

fn redraw(surface: &str, t: &Tab) {
    let _ = rows::decorate(surface, decoration(t).as_ref());
}

fn poll(l: &mut Live) {
    let now = now_s();
    let due: Vec<(String, bool)> = l.tabs.iter().filter(|(_, t)| t.polling).map(|(s, t)| (s.clone(), t.pr.is_some())).collect();
    for (surface, has_pr) in due {
        let busy = l.jobs.values().any(|(s, _)| *s == surface);
        let t = &l.tabs[&surface];
        let waited = now - t.pushed_s;
        if waited > GIVE_UP_S || (t.summary.is_none() && waited > NO_CHECKS_S) {
            l.tabs.get_mut(&surface).unwrap().polling = false;
        } else if !busy {
            run(l, &surface, if has_pr { Command::GhPrChecks } else { Command::GhPrView });
        }
    }
    if !l.tabs.values().any(|t| t.polling) {
        timer::stop();
    }
}

/// What a finished gh run means for the tab.
fn finished(l: &mut Live, surface: &str, c: Command, out: &Output) {
    let s = l.settings.clone();
    let Some(t) = l.tabs.get_mut(surface) else { return };
    match c {
        // Without a PR for the branch yet, the next tick asks again, until NO_CHECKS_S.
        Command::GhPrView => {
            if let Ok(pr) = ci::parse_pr(out.stdout.as_bytes()) {
                (t.pr, t.url) = (Some(pr.number), Some(pr.url));
                run(l, surface, Command::GhPrChecks);
            }
        }
        Command::GhPrChecks => {
            let Ok(checks) = ci::parse_checks(out.stdout.as_bytes()) else { return };
            let Some(sum) = ci::summarize(&ci::since(checks, t.pushed_s)) else { return };
            let before = t.summary.as_ref().map(|x| x.verdict);
            let settled = sum.verdict != Verdict::Running;
            t.summary = Some(sum);
            t.polling = !settled;
            redraw(surface, t);
            if ci::ping(before, t.summary.as_ref().unwrap().verdict)
                && let Some((title, body)) = ping(t, &s)
                && let Err(e) = notify::notify(&title, &body, &actions(t), Some(surface))
            {
                log(LogLevel::Warn, &format!("ci-lights: no notification: {e}"));
            }
        }
        _ => {}
    }
}

struct Plugin;

impl Guest for Plugin {
    fn init(json: String) {
        with(|l| l.settings = serde_json::from_str(&json).unwrap_or_default());
    }

    fn on_settings_changed(json: String) {
        with(|l| l.settings = serde_json::from_str(&json).unwrap_or_default());
    }

    fn on_event(event: AgentEvent) {
        // Claude's finished call is `tool_ran`, Codex's `tool_done`.
        if !matches!(event.kind.as_str(), "tool_ran" | "tool_done") || event.agent_id.is_some() {
            return;
        }
        let (Some(surface), Some(command)) = (event.surface.as_deref(), event.command.as_deref()) else { return };
        if !ci::is_push(command) {
            return;
        }
        with(|l| {
            if !event.verified && !l.settings.unverified {
                return;
            }
            let name = event.cwd.as_deref().map(name_of).unwrap_or_default();
            l.tabs.insert(surface.to_owned(), Tab { name, pushed_s: now_s(), pr: None, url: None, summary: None, polling: true });
            let _ = rows::decorate(surface, None);
            run(l, surface, Command::GhPrView);
            if let Err(e) = timer::every(l.settings.poll_secs.clamp(15, 600) as u32) {
                log(LogLevel::Warn, &format!("ci-lights: no timer: {e}"));
            }
        });
    }

    fn palette_items(_mode: String, _query: String) -> Vec<PaletteItem> {
        Vec::new()
    }

    // "Open agent" brings the tab up through the notification's binding; "Open PR" is the PR's url.
    fn on_action(action_id: String, _payload: String) {
        if action_id.starts_with("https://")
            && let Err(e) = link::open_url(&action_id)
        {
            log(LogLevel::Warn, &format!("ci-lights: PR not opened: {e}"));
        }
    }

    fn row_menu(_surface: String) -> Vec<MenuItem> {
        Vec::new()
    }

    fn on_row_moved(_surface: String, _group: String, _before: Option<String>) {}

    fn on_theme_changed() {}

    fn on_tick(_now_ms: u64) {
        with(poll);
    }

    fn on_exec(job: u64, outcome: Result<Output, String>) {
        with(|l| {
            let Some((surface, c)) = l.jobs.remove(&job) else { return };
            let what = match &outcome {
                Ok(out) if out.status == 0 => None,
                Ok(out) => Some(format!("exited {}: {}", out.status, out.stderr.lines().next().unwrap_or(""))),
                Err(e) => Some(e.clone()),
            };
            complain(l, &surface, what);
            if let Ok(out) = outcome {
                finished(l, &surface, c, &out);
            }
        });
    }

    fn on_away_changed(_away: bool, _away_ms: u64) {}

    fn on_hover_hold(_held: bool, _surface: String) {}

    fn on_fs_tree(_job: u64, _outcome: Result<haunt::plugin::fs_list::TreeResult, String>) {}
}

export!(Plugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(verdict: Verdict) -> Tab {
        let summary = Summary { done: 7, total: 9, verdict, failed: Some(("lint (clippy)".into(), Some(192))), took: None };
        Tab { name: "api".into(), pushed_s: 0, pr: Some(142), url: Some("https://github.com/o/api/pull/142".into()), summary: Some(summary), polling: true }
    }

    #[test]
    fn the_row_reads_like_the_mockup() {
        let d = decoration(&tab(Verdict::Running)).unwrap();
        assert_eq!((d.label.as_deref(), d.suffix.as_deref()), (Some("CI 7/9"), Some("#142 · checks running")));
        assert!(matches!((d.label_tone, d.suffix_tone), (Tone::Text, Tone::Dim)));
        assert!(matches!(decoration(&tab(Verdict::Failed)).unwrap().label_tone, Tone::Warn), "red CI is the warn orange");
        assert!(decoration(&Tab { pr: None, ..tab(Verdict::Running) }).is_none(), "nothing until the PR is known");
    }

    #[test]
    fn the_notification_reads_like_the_mockup() {
        let s = Settings::default();
        assert_eq!(ping(&tab(Verdict::Failed), &s), Some(("api · #142 checks failed".into(), "lint (clippy) failed after 3m12s".into())));
        assert_eq!(ping(&tab(Verdict::Running), &s), None);
        assert_eq!(ping(&tab(Verdict::Failed), &Settings { notify_failed: false, ..s }), None);
    }

    #[test]
    fn the_notification_opens_the_pr_or_the_agent() {
        let ids = |t: &Tab| actions(t).into_iter().map(|a| (a.id, a.title)).collect::<Vec<_>>();
        assert_eq!(ids(&tab(Verdict::Failed)), [("https://github.com/o/api/pull/142".into(), "Open PR".into()), ("open".into(), "Open agent".into())]);
        let elsewhere = Tab { url: Some("https://gitlab.com/o/api/-/merge_requests/1".into()), ..tab(Verdict::Failed) };
        assert_eq!(ids(&elsewhere), [("open".to_owned(), "Open agent".to_owned())], "only github.com links are opened");
    }

    #[test]
    fn manifest_asks_for_what_it_uses_and_defaults_match() {
        let m = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/haunt-plugin.toml")).unwrap();
        let t: toml::Table = toml::from_str(&m).unwrap();
        let caps = t["capabilities"].as_table().unwrap();
        assert_eq!(caps.keys().collect::<Vec<_>>(), ["decorate", "event_details", "events", "exec", "notifications", "open_url", "timer"]);
        let defaults: serde_json::Map<String, serde_json::Value> =
            t["settings"].as_table().unwrap().iter().map(|(k, v)| (k.clone(), serde_json::to_value(&v["default"]).unwrap())).collect();
        assert_eq!(serde_json::from_value::<Settings>(serde_json::Value::Object(defaults)).unwrap(), Settings::default());
    }
}
