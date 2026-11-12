//! Reading `gh pr checks` and deciding what the row shows and when to ping, free of host calls.

use serde::Deserialize;

/// One entry of `gh pr checks --json name,bucket,startedAt,completedAt`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    /// `pass`, `fail`, `pending`, `skipping` or `cancel`.
    pub bucket: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// `gh pr view --json number,url`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub url: String,
}

pub fn parse_checks(json: &[u8]) -> Result<Vec<Check>, String> {
    serde_json::from_slice(json).map_err(|e| e.to_string())
}

pub fn parse_pr(json: &[u8]) -> Result<Pr, String> {
    let pr: Pr = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    if !pr.url.starts_with("https://") || pr.url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("unexpected PR url".into());
    }
    Ok(pr)
}

/// A `git push` anywhere in a shell command line, not `git push --dry-run`.
pub fn is_push(command: &str) -> bool {
    command.split(['&', ';', '|', '\n']).any(|part| {
        let words: Vec<&str> = part.split_whitespace().collect();
        let Some(g) = words.iter().position(|w| *w == "git") else { return false };
        let rest = &words[g + 1..];
        let mut i = 0;
        while i < rest.len() && rest[i].starts_with('-') {
            i += if matches!(rest[i], "-C" | "-c") { 2 } else { 1 };
        }
        rest.get(i) == Some(&"push") && !rest.iter().any(|w| *w == "--dry-run" || *w == "-n")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Running,
    Failed,
    Passed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub done: usize,
    pub total: usize,
    pub verdict: Verdict,
    /// The first failed check and how long it ran.
    pub failed: Option<(String, Option<u64>)>,
    /// From the first start to the last finish, once everything is done.
    pub took: Option<u64>,
}

/// Skipped checks don't count. Any failure is red at once; green waits for every check.
pub fn summarize(checks: &[Check]) -> Option<Summary> {
    let counted: Vec<&Check> = checks.iter().filter(|c| c.bucket != "skipping").collect();
    if counted.is_empty() {
        return None;
    }
    let done = counted.iter().filter(|c| c.bucket != "pending").count();
    let failed = counted.iter().find(|c| c.bucket == "fail" || c.bucket == "cancel").map(|c| (clean(&c.name), span(c.started_at.as_deref(), c.completed_at.as_deref())));
    let verdict = if failed.is_some() {
        Verdict::Failed
    } else if done == counted.len() {
        Verdict::Passed
    } else {
        Verdict::Running
    };
    let took = (done == counted.len())
        .then(|| {
            let start = counted.iter().filter_map(|c| c.started_at.as_deref().and_then(epoch_s)).min()?;
            let end = counted.iter().filter_map(|c| c.completed_at.as_deref().and_then(epoch_s)).max()?;
            (end >= start).then(|| (end - start) as u64)
        })
        .flatten();
    Some(Summary { done, total: counted.len(), verdict, failed, took })
}

/// Checks of this push: a run that started well before it belongs to the commit before.
pub fn since(checks: Vec<Check>, pushed_s: i64) -> Vec<Check> {
    checks.into_iter().filter(|c| c.started_at.as_deref().and_then(epoch_s).is_none_or(|t| t >= pushed_s - 30)).collect()
}

/// A ping when the checks settle: red as soon as one fails, green once all pass. Once per push.
pub fn ping(before: Option<Verdict>, now: Verdict) -> bool {
    now != Verdict::Running && before != Some(now) && !matches!(before, Some(Verdict::Failed | Verdict::Passed))
}

fn span(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let (s, e) = (epoch_s(start?)?, epoch_s(end?)?);
    (e >= s).then(|| (e - s) as u64)
}

/// Seconds since the epoch of an RFC 3339 UTC time as GitHub sends it, `2026-11-12T10:04:05Z`.
pub fn epoch_s(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || !t.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// `45s`, `3m12s`, `1h4m`.
pub fn duration(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 if s.is_multiple_of(60) => format!("{}m", s / 60),
        60..3600 => format!("{}m{}s", s / 60, s % 60),
        _ => format!("{}h{}m", s / 3600, s % 3600 / 60),
    }
}

/// Check names come from the repo's workflows: untrusted.
pub fn clean(s: &str) -> String {
    let one: String = s.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).filter(|c| !c.is_control() && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')).collect();
    let one = one.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 48 { one } else { format!("{}…", one.chars().take(47).collect::<String>()) }
}

impl Summary {
    /// The row's right-hand label, `CI 7/9`.
    pub fn label(&self) -> String {
        format!("CI {}/{}", self.done, self.total)
    }

    /// The meta suffix after the branch: `#142 · checks running`.
    pub fn suffix(&self, pr: u64) -> String {
        let what = match self.verdict {
            Verdict::Running => "checks running",
            Verdict::Failed => "checks failed",
            Verdict::Passed => "checks passed",
        };
        format!("#{pr} · {what}")
    }

    /// The notification body.
    pub fn body(&self) -> String {
        match (&self.failed, self.verdict) {
            (Some((name, Some(s))), _) => format!("{name} failed after {}", duration(*s)),
            (Some((name, None)), _) => format!("{name} failed"),
            (None, Verdict::Passed) => match self.took {
                Some(s) => format!("all {} passed in {}", self.total, duration(s)),
                None => format!("all {} passed", self.total),
            },
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, bucket: &str, start: Option<&str>, end: Option<&str>) -> Check {
        Check { name: name.into(), bucket: bucket.into(), started_at: start.map(Into::into), completed_at: end.map(Into::into) }
    }

    #[test]
    fn reads_gh_json() {
        let j = br#"[{"name":"lint (clippy)","bucket":"fail","startedAt":"2026-11-12T10:00:00Z","completedAt":"2026-11-12T10:03:12Z","state":"FAILURE"}]"#;
        let c = parse_checks(j).unwrap();
        assert_eq!(c[0].name, "lint (clippy)");
        assert!(parse_checks(b"nope").is_err());
        assert_eq!(parse_pr(br#"{"number":142,"url":"https://github.com/o/r/pull/142"}"#).unwrap().number, 142);
        assert!(parse_pr(br#"{"number":1,"url":"javascript:alert(1)"}"#).is_err());
    }

    #[test]
    fn pushes_are_spotted_in_command_lines() {
        for yes in ["git push", "git push -u origin feat/auth", "cargo test && git push", "git -C /w push origin"] {
            assert!(is_push(yes), "{yes}");
        }
        for no in ["git status", "git push --dry-run", "echo git", "git log --grep push", "gitpush"] {
            assert!(!is_push(no), "{no}");
        }
    }

    #[test]
    fn running_then_failed_then_passed() {
        let t = |m: u32, s: u32| format!("2026-11-12T10:{m:02}:{s:02}Z");
        let mut c: Vec<Check> = (0..9).map(|i| check(&format!("c{i}"), "pass", Some(&t(0, 0)), Some(&t(4, i)))).collect();
        c[7].bucket = "pending".into();
        c[8].bucket = "pending".into();
        c.push(check("docs", "skipping", None, None));
        let s = summarize(&c).unwrap();
        assert_eq!((s.label(), s.suffix(142), s.verdict), ("CI 7/9".into(), "#142 · checks running".into(), Verdict::Running));
        assert_eq!(s.took, None);

        c[8] = check("lint (clippy)", "fail", Some(&t(0, 0)), Some(&t(3, 12)));
        let s = summarize(&c).unwrap();
        assert_eq!((s.verdict, s.body()), (Verdict::Failed, "lint (clippy) failed after 3m12s".into()), "red before the rest finish");

        c[7].bucket = "pass".into();
        c[8].bucket = "pass".into();
        let s = summarize(&c).unwrap();
        assert_eq!((s.verdict, s.body()), (Verdict::Passed, "all 9 passed in 4m7s".into()));
    }

    #[test]
    fn nothing_to_show_without_counted_checks() {
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[check("x", "skipping", None, None)]), None);
    }

    #[test]
    fn timestamps_and_durations() {
        assert_eq!(epoch_s("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_s("2026-11-12T00:00:00Z"), Some(20_769 * 86_400));
        assert_eq!(epoch_s("2024-02-29T12:00:01Z"), Some(1_709_208_001));
        assert_eq!(epoch_s("2026-13-01T00:00:00Z"), None);
        assert_eq!(epoch_s("2026-11-12 00:00:00"), None);
        assert_eq!([5, 60, 192, 3840].map(duration), ["5s", "1m", "3m12s", "1h4m"]);
    }

    #[test]
    fn only_this_pushs_checks_count() {
        let c = vec![check("old", "pass", Some("2026-11-12T09:00:00Z"), Some("2026-11-12T09:01:00Z")), check("new", "pending", None, None), check("now", "pending", Some("2026-11-12T10:00:10Z"), None)];
        let kept: Vec<String> = since(c, epoch_s("2026-11-12T10:00:00Z").unwrap()).into_iter().map(|c| c.name).collect();
        assert_eq!(kept, ["new", "now"]);
    }

    #[test]
    fn one_ping_when_the_checks_settle() {
        assert!(!ping(None, Verdict::Running));
        assert!(ping(Some(Verdict::Running), Verdict::Failed));
        assert!(ping(None, Verdict::Passed));
        assert!(!ping(Some(Verdict::Failed), Verdict::Failed));
        assert!(!ping(Some(Verdict::Failed), Verdict::Passed), "a re-run going green after red is not news until the next push");
    }

    #[test]
    fn check_names_are_cleaned() {
        assert_eq!(clean("lint\u{202e} (clippy)\n"), "lint (clippy)");
    }
}
